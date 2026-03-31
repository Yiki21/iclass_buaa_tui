pkgname=iclass-buaa-tui
_binname=iclass_buaa_tui
pkgver=0.1.0
pkgrel=1
pkgdesc="BUAA class check-in terminal UI tool"
arch=('x86_64')
url="https://github.com/Yiki21/iclass_buaa_tui"
license=('MIT')
makedepends=('cargo')
source=("${pkgname}-${pkgver}.tar.gz")
sha256sums=('SKIP')

prepare() {
  cd "${srcdir}/${pkgname}-${pkgver}"
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

check() {
    export RUSTUP_TOOLCHAIN=stable
    cargo test --frozen --all-features
}

build() {
  cd "${srcdir}/${pkgname}-${pkgver}"
  export CARGO_TARGET_DIR=target
  cargo build --frozen --release --locked
}

package() {
  cd "${srcdir}/${pkgname}-${pkgver}"
  install -Dm755 "target/release/${_binname}" "${pkgdir}/usr/bin/${_binname}"
  install -Dm644 README.md "${pkgdir}/usr/share/doc/${pkgname}/README.md"
  install -Dm644 LICENSE "${pkgdir}/usr/share/licenses/${pkgname}/LICENSE"
}
