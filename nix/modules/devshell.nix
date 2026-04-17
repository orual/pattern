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

      # tidepool-extract is the GHC plugin binary that `tidepool-runtime`
      # (consumed by `pattern_runtime`) shells out to when compiling agent
      # Haskell programs. Provided by the tidepool flake input; exposed on
      # PATH via devshell packages below.
      tidepool-extract = inputs.tidepool.packages.${system}.tidepool-extract;
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

        # tidepool-runtime discovers the extractor via $TIDEPOOL_EXTRACT or
        # by looking up `tidepool-extract` on PATH. Exporting the absolute
        # path is belt-and-suspenders for workflows that don't inherit the
        # devshell PATH (e.g. nix-shell --command).
        TIDEPOOL_EXTRACT = "${tidepool-extract}/bin/tidepool-extract";

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
        ] ++ [
          # Tidepool GHC plugin binary (~300MB, GHC 9.12). Required at
          # runtime by `pattern_runtime`'s `compile_haskell` path.
          tidepool-extract
        ];
      };
    };
}
