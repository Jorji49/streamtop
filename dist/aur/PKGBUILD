# Maintainer: Ahmet Kayra Kama <kamaahmetkayra@gmail.com>
# Contributor: Ahmet Kayra Kama <kamaahmetkayra@gmail.com>
pkgname=streamtop-bin
pkgver=0.3.3
pkgrel=2
pkgdesc="Terminal diagnostic engine for live HLS, DASH, and IPTV streams"
arch=('x86_64')
url="https://github.com/Jorji49/streamtop"
license=('MIT')
depends=()
provides=('streamtop')
conflicts=('streamtop')
source=("${pkgname}-${pkgver}.tar.gz::https://github.com/Jorji49/streamtop/releases/download/v${pkgver}/streamtop-x86_64-unknown-linux-gnu.tar.gz"
        "LICENSE::https://raw.githubusercontent.com/Jorji49/streamtop/v${pkgver}/LICENSE")
sha256sums=('731952b8c1d355060bbea7fd236a3b4dfe64744f9f8fa52de29d3c9515cd3941'
            'SKIP')

package() {
  install -Dm755 "${srcdir}/streamtop" "${pkgdir}/usr/bin/streamtop"
  install -Dm644 "${srcdir}/LICENSE" "${pkgdir}/usr/share/licenses/${pkgname}/LICENSE"
}
