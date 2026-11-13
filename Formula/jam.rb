# Homebrew formula for the Jam compiler.
#
# Installation:
#
#   brew tap tri-berry/jam https://github.com/tri-berry/jam
#   brew install jam          # the latest release, built from source
#   brew install --HEAD jam   # the latest commit on main
#
# On a new release, update `url` and `sha256` to the tag's source tarball.

class Jam < Formula
  desc "Systems programming language with mutable value semantics"
  homepage "https://jamlang.org"
  url "https://github.com/tri-berry/jam/archive/refs/tags/v0.0.1.tar.gz"
  sha256 "486362b29acfcfbb9f50e1c54e7c3d99d8e01eaa2ea0cdaef60cd8c8cba184a8"
  license "Apache-2.0" => { with: "LLVM-exception" }
  head "https://github.com/tri-berry/jam.git", branch: "main"

  depends_on "rust" => :build
  depends_on "lld@22"
  depends_on "llvm@22"

  def install
    llvm = Formula["llvm@22"]
    # crates/jam-llvm/build.rs locates LLVM through this (before the
    # PATH / Homebrew fallbacks) and links the monolithic libLLVM.
    ENV["LLVM_CONFIG"] = (llvm.opt_bin/"llvm-config").to_s
    # jam links programs with the LLD it embeds.
    ENV["JAM_LLD_PREFIX"] = formula_opt_prefix("lld@22").to_s

    system "cargo", "install", *std_cargo_args(path: "crates/jam")

    (lib/"jam").install "std" unless (lib/"jam/std").exist?
    # The system-library stub programs link against, so no Xcode tools
    # are needed to build a jam program.
    (lib/"jam").install "libc" unless (lib/"jam/libc").exist?
  end

  test do
    # Verify the std lookup works end-to-end: the installed `jam` walks
    # up from `#{prefix}/bin` and finds `#{prefix}/lib/jam/std/`, so
    # `import("std/collections")` resolves without any flag.
    (testpath/"hello.jam").write <<~JAM
      const { Vec } = import("std/collections");
      const { Option } = import("std/option");
      fn main() {
        var v: Vec(i32) = Vec(i32).empty();
        v.push(42);
      }
    JAM
    system bin/"jam", testpath/"hello.jam"
    assert_path_exists testpath/"output"
  end
end
