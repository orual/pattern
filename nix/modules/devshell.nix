{inputs, ...}: {
  perSystem = {
    config,
    self',
    pkgs,
    lib,
    system,
    ...
  }: let
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
  in {
    devShells.default = pkgsWithUnfree.mkShell {
      name = "pattern-shell";
      inputsFrom = [
        self'.devShells.rust

        config.pre-commit.devShell # See ./nix/modules/pre-commit.nix
      ];
      RUST_BACKTRACE = 0;
      MEMORY_DIR = "./.pattern/shared";

      # tidepool-runtime discovers the extractor via $TIDEPOOL_EXTRACT or
      # by looking up `tidepool-extract` on PATH. Exporting the absolute
      # path is belt-and-suspenders for workflows that don't inherit the
      # devshell PATH (e.g. nix-shell --command).
      TIDEPOOL_EXTRACT = "${tidepool-extract}/bin/tidepool-extract";
      LIBCLANG_PATH = "${pkgs.llvmPackages_18.libclang.lib}/lib";

      packages = with pkgsWithUnfree;
        [
          just
          nixd # Nix language server
          bacon
          rust-analyzer
          clang
          lazysql
          pkg-config
          cargo-expand
          jujutsu
          cmake
          pkg-config
          llama-cpp-vulkan
          cargo-nextest
          git
          gh
          haskellPackages.lsp
          sqlx-cli
          # pattern-provider deps: keyring (Secret Service) needs libdbus.
          dbus
          openssl
          vulkan-headers
          vulkan-loader
          shaderc
          llvmPackages_18.libclang
        ]
        ++ [
          # Tidepool GHC plugin binary (~300MB, GHC 9.12). Required at
          # runtime by `pattern_runtime`'s `compile_haskell` path.
          tidepool-extract
        ];
    };
  };
}
