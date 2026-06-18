# Maintainer: Watchtower <https://github.com/I-XXII-V/Watchtower>
# Contributor: I-XXII-V

pkgname=watchtower
pkgver=0.1.0
pkgrel=1
pkgdesc="CLI tool to check the health of your dependencies across AUR, Cargo, npm, PyPI, and Go"
arch=('x86_64')
url="https://github.com/I-XXII-V/Watchtower"
license=('MIT')
makedepends=('cargo')
source=("$url/archive/v$pkgver.tar.gz")
sha256sums=('ed7ce92712c5788e98a164b5cf5709bd28da835436e01626873b8108fd95b62a')

build() {
    cd "$srcdir/$pkgname-$pkgver"
    cargo build --release --locked
}

package() {
    cd "$srcdir/$pkgname-$pkgver"
    install -Dm755 target/release/watchtower "$pkgdir/usr/bin/watchtower"
    install -Dm644 README.md "$pkgdir/usr/share/doc/$pkgname/README.md"
    install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
