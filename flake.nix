{
  description = "A basic flake with a shell";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    # cargo-workspace.url = "github:maix-flake/cargo-ws";
    # cargo-semver-checks.url = "github:Maix0/cargo-semver-checks-flake";
  };
  outputs = {
    self,
    nixpkgs,
    flake-utils,
    rust-overlay,
    ...
  } @ inputs:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };
      packageIf = name: packageDef:
        if builtins.hasAttr name inputs
        then [(packageDef inputs.${name})]
        else [];
    in {
      devShell = let
        rust_bin =
          pkgs.rust-bin.stable.latest.default;
      in
        pkgs.mkShell {
          packages =
            [rust_bin]
            ++ (packageIf "cargo-semver-checks" (p: p.packages.${system}.default))
            ++ (packageIf "cargo-workspace" (p: p.packages.${system}.default));

          shellHook = ''
            export RUST_STD="${rust_bin}/share/doc/rust/html/std/index.html"
          '';
        };
    });
}
