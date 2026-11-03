//! Copyable argv: no shell expansion on the machine where the command is pasted.
use super::*;
use gpui_kit::component::WindowExt as _;

#[derive(Clone, Copy)]
enum Quoting {
    Posix,
    PowerShell,
}
fn quote(value: &str, style: Quoting) -> String {
    if !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || match style {
                    Quoting::Posix => b"_@%+:,./-".contains(&byte),
                    Quoting::PowerShell => b"_./:-".contains(&byte),
                }
        })
    {
        return value.to_owned();
    }
    match style {
        Quoting::Posix => format!("'{}'", value.replace('\'', "'\\''")),
        Quoting::PowerShell => format!("'{}'", value.replace('\'', "''")),
    }
}
fn format_command(argv: &[String], style: Quoting) -> String {
    argv.iter()
        .map(|arg| quote(arg, style))
        .collect::<Vec<_>>()
        .join(" ")
}
impl TerminalView {
    fn kubectl_command(&self, cx: &App) -> Result<String, &'static str> {
        let kubeconfig = crate::sources::kubeconfig_path(self.session.id(), cx);
        if self.session.id().as_str() != self.session.id().context_name() && kubeconfig.is_none() {
            return Err(
                "The kubeconfig source is unavailable. Restore it before copying this command.",
            );
        }
        let mut args = vec![
            "kubectl".into(),
            "exec".into(),
            "-it".into(),
            self.pod.clone(),
            "-n".into(),
            self.namespace.clone(),
            "--context".into(),
            self.session.id().context_name().into(),
        ];
        if let Some(path) = kubeconfig {
            args.extend(["--kubeconfig".into(), path.to_string_lossy().into_owned()]);
        }
        if let Some(container) = &self.container {
            args.extend(["-c".into(), container.clone()]);
        }
        args.push("--".into());
        args.extend(self.command(cx));
        Ok(format_command(
            &args,
            if cfg!(windows) {
                Quoting::PowerShell
            } else {
                Quoting::Posix
            },
        ))
    }
    pub(super) fn copy_kubectl_command(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.kubectl_command(cx) {
            Ok(command) => {
                cx.write_to_clipboard(ClipboardItem::new_string(command));
                window.push_notification(crate::i18n::text("kubectl command copied."), cx);
            }
            Err(message) => window.push_notification(crate::i18n::text(message), cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[::core::prelude::v1::test]
    fn quotes_preserve_shell_scripts_empty_arguments_and_special_characters() {
        assert_eq!(quote("bash", Quoting::Posix), "bash");
        assert_eq!(quote("", Quoting::Posix), "''");
        assert_eq!(quote("O'Brien", Quoting::Posix), "'O'\\''Brien'");
        assert_eq!(quote("O'Brien", Quoting::PowerShell), "'O''Brien'");
        assert_eq!(quote("@args", Quoting::PowerShell), "'@args'");
        assert_eq!(
            quote("$HOME; $(echo wrong)", Quoting::PowerShell),
            "'$HOME; $(echo wrong)'"
        );
        #[cfg(unix)]
        {
            let values = [
                "bash",
                "=ls",
                "",
                "a b",
                "O'Brien",
                "$HOME; $(echo wrong)",
                "中文",
                "echo \"quoted\"\nnext",
            ];
            let args = values.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            // Parse as argv without executing any of the copied arguments.
            let script = format!(
                "set -- {}; printf '%s\\0' \"$@\"",
                format_command(&args, Quoting::Posix)
            );
            let output = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(script)
                .output()
                .unwrap();
            assert!(output.status.success());
            let parsed = output
                .stdout
                .split(|b| *b == 0)
                .take(values.len())
                .collect::<Vec<_>>();
            assert_eq!(
                parsed,
                values.iter().map(|s| s.as_bytes()).collect::<Vec<_>>()
            );
        }
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests;
