# Maintainer: Ahmet Kayra Kama <kayraak@pm.me>
# Contributor: Ahmet Kayra Kama <kayraak@pm.me>
pkgname=streamtop-bin
pkgver=1.1.2
pkgrel=1
pkgdesc="Terminal HLS, DASH, and IPTV stream monitor with wire probes and metrics"
arch=('x86_64')
url="https://github.com/Jorji49/streamtop"
license=('MIT')
depends=()
provides=('streamtop')
conflicts=('streamtop')
source=("${pkgname}-${pkgver}.tar.gz::https://github.com/Jorji49/streamtop/releases/download/v${pkgver}/streamtop-x86_64-unknown-linux-gnu.tar.gz"
        "LICENSE::https://raw.githubusercontent.com/Jorji49/streamtop/v${pkgver}/LICENSE")
sha256sums=('5624f1586e90faed8e4234f555414dba7b7b6394694db9738448ddbfd4127325'
            '17ae2f4382f88ddcc4dafa488d59401dbb0d3a59d7f1a967c715cd13fbfa9f3d')

package() {
  install -Dm755 "${srcdir}/streamtop" "${pkgdir}/usr/bin/streamtop"
  install -Dm644 "${srcdir}/LICENSE" "${pkgdir}/usr/share/licenses/${pkgname}/LICENSE"
}
