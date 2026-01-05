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
        RUST_BACKTRACE = 0;
        CARGO_MOMMYS_LITTLE = "girl/pet/entity/baby";
        CARGO_MOMMYS_PRONOUNS = "her/their";
        CARGO_MOMMYS_MOODS = "chill/ominous/thirsty/yikes";
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
          cargo-nextest
          git
          gh
          sqlx-cli
        ];
      };
    };
}
