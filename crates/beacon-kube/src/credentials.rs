//! Windows GUI policy for noninteractive EKS token commands.
use kube::config::{AuthInfo, ExecInteractiveMode};

pub(crate) fn prepare(auth: &mut AuthInfo) {
    let Some(exec) = &mut auth.exec else {
        return;
    };
    // Respect explicitly interactive plugins, even when their command is AWS.
    if exec.interactive_mode == Some(ExecInteractiveMode::Always) {
        return;
    }
    let Some(command) = &exec.command else {
        return;
    };
    // Kubeconfig may use an absolute Windows path, with either separator.
    let program = command.rsplit(['/', '\\']).next().unwrap_or(command);
    let Some(args) = &exec.args else {
        return;
    };
    let token_command =
        if program.eq_ignore_ascii_case("aws") || program.eq_ignore_ascii_case("aws.exe") {
            args.windows(2).any(|args| args == ["eks", "get-token"])
        } else if program.eq_ignore_ascii_case("aws-iam-authenticator")
            || program.eq_ignore_ascii_case("aws-iam-authenticator.exe")
        {
            args.iter().any(|arg| arg == "token")
        } else {
            false
        };
    if token_command {
        // kube-rs otherwise treats an absent/IfAvailable mode as interactive,
        // despite this GUI having no terminal. Capture AWS stderr for diagnosis
        // and let the patched client suppress the credential process's console.
        exec.interactive_mode = Some(ExecInteractiveMode::Never);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(command: &str, args: &[&str], mode: Option<ExecInteractiveMode>) -> AuthInfo {
        let mut auth: AuthInfo = serde_json::from_value(serde_json::json!({
            "exec": {
                "apiVersion": "client.authentication.k8s.io/v1beta1",
                "command": command,
                "args": args,
                "env": [{"name": "AWS_PROFILE", "value": "development"}]
            }
        }))
        .unwrap();
        auth.exec.as_mut().unwrap().interactive_mode = mode;
        auth
    }

    #[test]
    fn aws_eks_credentials_are_noninteractive_without_changing_arguments_or_environment() {
        for command in [
            "aws",
            "aws.exe",
            r"C:\Program Files\Amazon\AWSCLIV2\aws.exe",
            "C:/AWS/AWS.EXE",
        ] {
            for mode in [
                None,
                Some(ExecInteractiveMode::IfAvailable),
                Some(ExecInteractiveMode::Never),
            ] {
                let mut auth = auth(
                    command,
                    &[
                        "--region",
                        "cn-north-1",
                        "eks",
                        "get-token",
                        "--cluster-name",
                        "demo",
                    ],
                    mode,
                );
                let before = auth.exec.clone().unwrap();
                prepare(&mut auth);
                let after = auth.exec.unwrap();
                assert_eq!(after.interactive_mode, Some(ExecInteractiveMode::Never));
                assert_eq!(after.command, before.command);
                assert_eq!(after.args, before.args);
                assert_eq!(after.env, before.env);
            }
        }
    }

    #[test]
    fn interactive_plugins_and_other_commands_keep_their_original_mode() {
        for (command, args, mode) in [
            (
                "kubelogin.exe",
                vec!["get-token"],
                Some(ExecInteractiveMode::IfAvailable),
            ),
            (
                "aws.exe",
                vec!["eks", "get-token"],
                Some(ExecInteractiveMode::Always),
            ),
            ("aws.exe", vec!["sso", "login"], None),
            ("aws.exe", vec!["get-token"], None),
            ("other.exe", vec!["eks", "get-token"], None),
        ] {
            let mut auth = auth(command, &args, mode.clone());
            prepare(&mut auth);
            assert_eq!(auth.exec.unwrap().interactive_mode, mode);
        }
        prepare(&mut AuthInfo::default());
    }

    #[test]
    fn legacy_eks_iam_authenticator_tokens_are_also_noninteractive() {
        let mut auth = auth(
            r"C:\AWS\aws-iam-authenticator.exe",
            &["token", "-i", "demo"],
            None,
        );
        prepare(&mut auth);
        assert_eq!(
            auth.exec.unwrap().interactive_mode,
            Some(ExecInteractiveMode::Never)
        );
    }

    #[cfg(windows)]
    fn windows_client(script: &str) -> Result<kube::Client, kube::Error> {
        let mut config = kube::Config::new("http://127.0.0.1:9".parse().unwrap());
        config.auth_info = auth(
            "powershell.exe",
            &["-NoProfile", "-NonInteractive", "-Command", script],
            Some(ExecInteractiveMode::Never),
        );
        // Client construction runs the plugin but does not contact a cluster.
        kube::Client::try_from(config)
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn noninteractive_credentials_run_without_a_windows_console() {
        windows_client(
            r#"
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class ConsoleWindow { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }'
if ([ConsoleWindow]::GetConsoleWindow() -ne [IntPtr]::Zero) { [Console]::Error.WriteLine('unexpected console window'); exit 1 }
$info = $env:KUBERNETES_EXEC_INFO | ConvertFrom-Json
if ($info.spec.interactive) { [Console]::Error.WriteLine('unexpected interactive credential request'); exit 1 }
'{"apiVersion":"client.authentication.k8s.io/v1beta1","kind":"ExecCredential","status":{"token":"fixture-token"}}'
"#,
        )
        .expect("a noninteractive plugin should return credentials without a console window");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn hidden_credentials_preserve_failure_diagnostics() {
        let error =
            windows_client("[Console]::Error.WriteLine('beacon-credential-fixture-error'); exit 1")
                .err()
                .expect("the failing credential plugin should report an error");
        assert!(crate::error::diagnose(&error).contains("beacon-credential-fixture-error"));
    }
}
