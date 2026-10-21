set -eu
control=$1
umask 077
mkdir "$control"
trap 'rm -f "$control/stop"; rmdir "$control" 2>/dev/null || :' 0
while [ ! -f "$control/stop" ]; do
    sleep 1
done
