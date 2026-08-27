# Maintainer: Ahmet Kayra Kama <kamaahmetkayra@gmail.com>
# Contributor: Ahmet Kayra Kama <kamaahmetkayra@gmail.com>
pkgname=streamtop-bin
pkgver=0.3.4
pkgrel=1
pkgdesc="HLS/DASH/IPTV stream diagnostics in the terminal"
arch=('x86_64')
url="https://github.com/Jorji49/streamtop"
license=('MIT')
depends=()
provides=('streamtop')
conflicts=('streamtop')
source=("${pkgname}-${pkgver}.tar.gz::https://github.com/Jorji49/streamtop/releases/download/v${pkgver}/streamtop-x86_64-unknown-linux-gnu.tar.gz"
        "LICENSE::https://raw.githubusercontent.com/Jorji49/streamtop/v${pkgver}/LICENSE")
sha256sums=('0e1140ec9bd0fd3e627d51470b664506c172a5e98f78df1483cf3b5849358e2a'
            'SKIP')

package() {
  install -Dm755 "${srcdir}/streamtop" "${pkgdir}/usr/bin/streamtop"
  install -Dm644 "${srcdir}/LICENSE" "${pkgdir}/usr/share/licenses/${pkgname}/LICENSE"
}
