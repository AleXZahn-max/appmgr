# Maintainer: Alex Zahn
pkgname=appmgr
pkgver=2.0.1
pkgrel=1
pkgdesc="Universal Linux Application Combine (.deb, .AppImage, .tar.*, .zip, Standalone ELF)"
arch=('x86_64' 'aarch64')
url="https://github.com/AleXZahn-max/appmgr"
license=('MIT')
depends=('bubblewrap' 'tar' 'xz' 'zstd')
makedepends=('cargo')
optdepends=(
    'fuse2: native AppImage mounting'
    'gtk-update-icon-cache: automatic icon cache regeneration'
    'desktop-file-utils: automatic desktop database indexing'
    'xdg-desktop-menu: XDG desktop integration'
)
source=()

build() {
    cd "$startdir"
    unset CFLAGS CXXFLAGS LDFLAGS RUSTFLAGS
    cargo build --release --locked
}

package() {
    cd "$startdir"
    install -Dm755 target/release/appmgr "$pkgdir/usr/bin/appmgr"

    # Shell completions
    install -d "$pkgdir/usr/share/bash-completion/completions"
    install -d "$pkgdir/usr/share/zsh/site-functions"
    install -d "$pkgdir/usr/share/fish/vendor_completions.d"

    target/release/appmgr completions bash > "$pkgdir/usr/share/bash-completion/completions/appmgr"
    target/release/appmgr completions zsh > "$pkgdir/usr/share/zsh/site-functions/_appmgr"
    target/release/appmgr completions fish > "$pkgdir/usr/share/fish/vendor_completions.d/appmgr.fish"

    # Man page
    if [ -f man/man1/appmgr.1 ]; then
        install -Dm644 man/man1/appmgr.1 "$pkgdir/usr/share/man/man1/appmgr.1"
    fi

    # License
    install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
