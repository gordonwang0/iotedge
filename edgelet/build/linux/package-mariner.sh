#!/bin/bash

set -ex

# Get directory of running script
DIR="$(cd "$(dirname "$0")" && pwd)"

export DEBIAN_FRONTEND=noninteractive
export TZ=UTC


case "$AZURELINUX_RELEASE" in
    '2.0'*)
        UsePreview=n
        AzureLinuxIdentity=mariner2
        PackageExtension=cm2
        ;;
    '3.0'*)
        UsePreview=n
        AzureLinuxIdentity=azurelinux3
        PackageExtension=azl3
        ;;
esac

BUILD_REPOSITORY_LOCALPATH="$(realpath "${BUILD_REPOSITORY_LOCALPATH:-$DIR/../../..}")"
EDGELET_ROOT="$BUILD_REPOSITORY_LOCALPATH/edgelet"
AZURELINUX_BUILD_ROOT="$BUILD_REPOSITORY_LOCALPATH/builds/$AzureLinuxIdentity"
REVISION="${REVISION:-1}"

apt-get update -y
apt-get upgrade -y

if [ "${AZURELINUX_RELEASE:0:3}" = '2.0' ]; then
    apt-get install -y software-properties-common
    add-apt-repository -y ppa:longsleep/golang-backports
    apt-get update -y
fi

apt-get install -y \
    acl cmake cpio curl g++ gcc genisoimage git golang-1.21-go jq libclang1 libssl-dev \
    llvm-dev make pigz pkg-config python3-distutils python3-pip qemu-utils rpm tar \
    wget zstd

rm -f /usr/bin/go
ln -vs /usr/lib/go-1.21/bin/go /usr/bin/go
touch /.mariner-toolkit-ignore-dockerenv

# Build Azure Linux toolkit
mkdir -p "$AZURELINUX_BUILD_ROOT"
AzureLinuxToolkitDir='/tmp/azurelinux'
if ! [ -f "$AzureLinuxToolkitDir/toolkit.tar.gz" ]; then
    rm -rf "$AzureLinuxToolkitDir"
    git clone 'https://github.com/microsoft/azurelinux.git' --branch "$AZURELINUX_RELEASE" --depth 1 "$AzureLinuxToolkitDir"
    pushd "$AzureLinuxToolkitDir/toolkit/"
    make package-toolkit REBUILD_TOOLS=y
    popd
    cp "$AzureLinuxToolkitDir"/out/toolkit-*.tar.gz "$AZURELINUX_BUILD_ROOT/toolkit.tar.gz"
    rm -rf $AzureLinuxToolkitDir
fi

echo 'Installing rustup'
curl -sSLf https://sh.rustup.rs | sh -s -- -y
. ~/.cargo/env

pushd $EDGELET_ROOT
case "$ARCH" in
    'x86_64')
        rustup target add x86_64-unknown-linux-gnu
        ;;
    'aarch64')
        rustup target add aarch64-unknown-linux-gnu
        ;;
esac
popd

# get aziot-identity-service version
IIS_VERSION=$(
    rpm -qp --queryformat '%{Version}' $(ls /src/aziot-identity-service-*.$PackageExtension.$ARCH.rpm | head -1)
)

# Update versions in specfiles
pushd "$BUILD_REPOSITORY_LOCALPATH"
sed -i "s/@@VERSION@@/$VERSION/g" $BUILD_REPOSITORY_LOCALPATH/builds/mariner/SPECS/aziot-edge/aziot-edge.signatures.json
sed -i "s/@@VERSION@@/$VERSION/g" $BUILD_REPOSITORY_LOCALPATH/builds/mariner/SPECS/aziot-edge/aziot-edge.spec
sed -i "s/@@RELEASE@@/$REVISION/g" $BUILD_REPOSITORY_LOCALPATH/builds/mariner/SPECS/aziot-edge/aziot-edge.spec

# Update aziot-identity-service version dependency
if [[ ! -z $IIS_VERSION ]]; then
    sed -i "s/@@IIS_VERSION@@/$IIS_VERSION/g" $BUILD_REPOSITORY_LOCALPATH/builds/mariner/SPECS/aziot-edge/aziot-edge.spec
else
    # if a version could not be parsed remove the version dependency
    sed -i "s/aziot-identity-service = @@IIS_VERSION@@%{?dist}/aziot-identity-service/g" $BUILD_REPOSITORY_LOCALPATH/builds/mariner/SPECS/aziot-edge/aziot-edge.spec
fi
popd

pushd "$EDGELET_ROOT"

# Cargo vendored dependencies are being downloaded now to be cached for Azure Linux iotedge build.
echo "set cargo home location"
mkdir $BUILD_REPOSITORY_LOCALPATH/cargo-home
export CARGO_HOME=$BUILD_REPOSITORY_LOCALPATH/cargo-home

echo "Vendoring Rust dependencies"
cargo vendor vendor

# Configure Cargo to use vendored the deps
mkdir -p .cargo
cat > .cargo/config << EOF
[source.crates-io]
replace-with = "vendored-sources"

[source."https://github.com/Azure/iot-identity-service"]
git = "https://github.com/Azure/iot-identity-service"
branch = "main"
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF

# Include license file directly, since parent dir will not be present in the tarball
rm ./LICENSE
cp ../LICENSE ./LICENSE

popd # EDGELET_ROOT

# Create source tarball, including cargo dependencies and license
tmp_dir="$(mktemp -d -t azurelinux-iotedge-build-XXXXXXXXXX)"
pushd "$tmp_dir"
echo "Creating source tarball aziot-edge-$VERSION.tar.gz"
tar -czvf aziot-edge-$VERSION.tar.gz --transform s/./aziot-edge-$VERSION/ -C "$BUILD_REPOSITORY_LOCALPATH" .
popd


# Copy source tarball to expected locations
mkdir -p "$AZURELINUX_BUILD_ROOT/SPECS/aziot-edge/SOURCES/"
mv "$tmp_dir/aziot-edge-$VERSION.tar.gz" "$AZURELINUX_BUILD_ROOT/SPECS/aziot-edge/SOURCES/"
rm -rf "$tmp_dir"

# Copy spec files to expected locations
cp "$BUILD_REPOSITORY_LOCALPATH/builds/mariner/SPECS/aziot-edge/aziot-edge.signatures.json" "$AZURELINUX_BUILD_ROOT/SPECS/aziot-edge/"
cp "$BUILD_REPOSITORY_LOCALPATH/builds/mariner/SPECS/aziot-edge/aziot-edge.spec" "$AZURELINUX_BUILD_ROOT/SPECS/aziot-edge/"

tmp_dir=$(mktemp -d)
pushd $tmp_dir
mkdir "rust"
cp -r ~/.cargo "rust"
cp -r ~/.rustup "rust"
tar cf "$AZURELINUX_BUILD_ROOT/SPECS/aziot-edge/SOURCES/rust.tar.gz" "rust"
popd

# copy over IIS RPM
mkdir -p $AZURELINUX_BUILD_ROOT/out/RPMS/$ARCH
mv /src/aziot-identity-service-*.$PackageExtension.$ARCH.rpm $AZURELINUX_BUILD_ROOT/out/RPMS/$ARCH

# Prepare toolkit
pushd $AZURELINUX_BUILD_ROOT
tar xzf toolkit.tar.gz
pushd toolkit

# Build Azure Linux RPM packages
make build-packages PACKAGE_BUILD_LIST="aziot-edge" SRPM_FILE_SIGNATURE_HANDLING=update USE_PREVIEW_REPO=$UsePreview CONFIG_FILE= -j$(nproc)
popd
popd

# Purge Cargo.lock and package-lock.json files from dependencies. If these files are present,
# Component Governance will incorrectly scan them for issues.
find "$CARGO_HOME/registry/src/" -name "Cargo.lock" -exec echo "Deleting {}" \; -exec rm {} \;
find "$EDGELET_ROOT/vendor/" -name "Cargo.lock" -exec echo "Deleting {}" \; -exec rm {} \;

find "$CARGO_HOME/registry/src/" -name "package-lock.json" -exec echo "Deleting {}" \; -exec rm {} \;
find "$EDGELET_ROOT/vendor/" -name "package-lock.json" -exec echo "Deleting {}" \; -exec rm {} \;
