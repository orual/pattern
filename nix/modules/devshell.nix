{ inputs, ... }: {
  perSystem =
    { config
    , self'
    , pkgs
    , lib
    , ...
    }: {
      devShells.default = pkgs.mkShell {
        name = "pattern-shell";
        inputsFrom = [
          self'.devShells.rust

          config.pre-commit.devShell # See ./nix/modules/pre-commit.nix
        ];
        packages = with pkgs; [
          just
          nixd # Nix language server
          bacon
          rust-analyzer
          clang
          pkg-config
          cargo-expand
        ];
      };
    };
}
