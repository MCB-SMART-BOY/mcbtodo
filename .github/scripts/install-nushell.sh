#!/usr/bin/env bash
set -euo pipefail

version="${NUSHELL_VERSION:?NUSHELL_VERSION is required}"
architecture="${1:?usage: install-nushell.sh <aarch64|x86_64>}"

case "$architecture" in
    aarch64)
        target="aarch64-unknown-linux-gnu"
        sha256="858142296927292095623c05c7ff0a79a53c9071949ca8867e672083ba87b73c"
        ;;
    x86_64)
        target="x86_64-unknown-linux-gnu"
        sha256="9c803a1b2019c80e75b4170edc2d3ccbf02aebc784bc9b243881ac178d27f381"
        ;;
    *)
        printf 'unsupported Nushell architecture: %s\n' "$architecture" >&2
        exit 2
        ;;
esac

sudo apt-get update
sudo apt-get install --yes bubblewrap

probe_bubblewrap() {
    /usr/bin/bwrap --unshare-user --disable-userns --unshare-pid \
        --unshare-net --unshare-ipc --unshare-uts --unshare-cgroup \
        --cap-drop ALL --die-with-parent --new-session \
        --ro-bind / / --chdir / -- /usr/bin/true
}

configure_runner_bubblewrap() {
    local restriction=/proc/sys/kernel/apparmor_restrict_unprivileged_userns
    [[ ${RUNNER_ENVIRONMENT:-} == github-hosted && -r "$restriction" && $(<"$restriction") == 1 ]] ||
        return 1

    # Ubuntu 24.04's Bubblewrap package does not ship a profile for this
    # runner policy. Grant user namespaces only to the system binary.
    sudo apparmor_parser --add <<'APPARMOR'
abi <abi/4.0>,
profile mcbtodo-ci-bwrap /usr/bin/bwrap flags=(unconfined) {
  userns,
}
APPARMOR
}

# Probe as the runner user with the same namespace flags used by the Worker.
if ! probe_bubblewrap && ! { configure_runner_bubblewrap && probe_bubblewrap; }; then
    printf 'Bubblewrap cannot start the required namespaces on this runner.\n' >&2
    uname -srmo >&2
    /usr/bin/bwrap --version >&2
    if [[ -r /proc/sys/kernel/apparmor_restrict_unprivileged_userns ]]; then
        sysctl kernel.apparmor_restrict_unprivileged_userns >&2
    fi
    sudo journalctl -k -b --no-pager --lines=300 |
        rg 'apparmor="DENIED".*(comm="bwrap"|unprivileged_userns)' >&2 || true
    exit 1
fi

archive="$RUNNER_TEMP/nu-$version-$target.tar.gz"
url="https://github.com/nushell/nushell/releases/download/$version/nu-$version-$target.tar.gz"
curl --fail --location --silent --show-error "$url" --output "$archive"
printf '%s  %s\n' "$sha256" "$archive" | sha256sum --check

extract_dir="$RUNNER_TEMP/nu-$version-$target"
mkdir -p "$extract_dir"
tar --extract --gzip --file "$archive" --strip-components=1 --directory "$extract_dir"
sudo install --mode 0755 "$extract_dir/nu" /usr/bin/nu
