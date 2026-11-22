# Maintainer: Alex Zahn
pkgname=appmgr
pkgver=1.4.0
pkgrel=1
pkgdesc="Universal Linux Application Combine (.deb, .AppImage, .tar.*, .zip, Standalone ELF)"
arch=('any')
url="https://github.com/AleXZahn-max/appmgr"
license=('MIT')
depends=('python' 'tar' 'desktop-file-utils')
optdepends=(
    'binutils: native AR archive support'
    'zstd: Zstandard compressed archives and packages support'
    'xz: XZ compressed archives support'
    'bubblewrap: sandboxed /opt path virtualization for heavy packages'
    'gtk-update-icon-cache: automatic icon cache regeneration'
    'xdg-desktop-menu: XDG desktop integration'
)
source=()

package() {
    cd "$startdir"
    make DESTDIR="$pkgdir" PREFIX="/usr" install
}
