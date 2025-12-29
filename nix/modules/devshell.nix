{ inputs, ... }: {
  perSystem =
    { config
    , self'
    , pkgs
    , lib
    , system
    , ...
    }:
    let
      # Create a custom pkgs instance that allows unfree packages
      pkgsWithUnfree = import inputs.nixpkgs {
        inherit system;
        config = {
          allowUnfree = true;
        };
      };
    in
    {
      devShells.default = pkgsWithUnfree.mkShell {
        name = "pattern-shell";
        inputsFrom = [
          self'.devShells.rust

          config.pre-commit.devShell # See ./nix/modules/pre-commit.nix
        ];
        packages = with pkgsWithUnfree; [
          just
          nixd # Nix language server
          bacon
          rust-analyzer
          clang
          lazysql
          pkg-config
          cargo-expand
          jujutsu
          git
          gh
          sqlx-cli
        ];
      };
    };
}
