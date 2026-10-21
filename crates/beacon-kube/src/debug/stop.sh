set -eu
control=$1
umask 077
for attempt in 1 2 3 4 5; do
    if [ -d "$control" ]; then
        : > "$control/stop"
        printf 'beacon-debug-stop-requested\n'
        exit 0
    fi
    sleep 1
done
printf 'The debug container stop control is unavailable.\n' >&2
exit 1
