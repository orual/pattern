{ inputs, ... }: {
  imports = [
    inputs.rust-flake.flakeModules.default
    inputs.rust-flake.flakeModules.nixpkgs
  ];
  debug = true;
  perSystem =
    { config
    , self'
    , pkgs
    , lib
    , system
    , ...
    }:
    let
      inherit (pkgs.stdenv) isDarwin;
      inherit (pkgs.darwin) apple_sdk;

      # Common configuration for all crates
      globalCrateConfig = {
        crane.clippy.enable = false;
      };

      # Common build inputs for all crates
      commonBuildInputs = lib.optionals isDarwin (
        with apple_sdk.frameworks; [
          IOKit
          Security
          SystemConfiguration
        ]
      );
    in
    {
      rust-project = {
        # Source filtering to avoid unnecessary rebuilds
        src = lib.cleanSourceWith {
          src = inputs.self;
          filter = config.rust-project.crane-lib.filterCargoSources;
        };

        # Define each workspace crate
        crates = {
          "pattern-core" = {
            imports = [ globalCrateConfig ];
            autoWire = [ "crate" "clippy" ];
            path = ./../../crates/pattern_core;
            crane = {
              args = {
                buildInputs = commonBuildInputs;
              };
            };
          };
          "pattern-db" = {
            imports = [ globalCrateConfig ];
            autoWire = [ "crate" "clippy" ];
            path = ./../../crates/pattern_db;
            crane = {
              args = {
                buildInputs =
                  commonBuildInputs
                  ++ [
                  ];
              };
            };
          };

          "pattern-nd" = {
            imports = [ globalCrateConfig ];
            autoWire = [ "crate" "clippy" ];
            path = ./../../crates/pattern_nd;
            crane = {
              args = {
                buildInputs = commonBuildInputs;
              };
            };
          };

          "pattern-mcp" = {
            imports = [ globalCrateConfig ];
            autoWire = [ "crate" "clippy" ];
            path = ./../../crates/pattern_mcp;
            crane = {
              args = {
                buildInputs = commonBuildInputs;
              };
            };
          };

          "pattern-cli" = {
            imports = [ globalCrateConfig ];
            autoWire = [ "crate" "clippy" ];
            path = ./../../crates/pattern_cli;
            crane = {
              args = {
                buildInputs =
                  commonBuildInputs
                  ++ [
                    pkgs.openssl
                    pkgs.pkg-config
                  ];
                nativeBuildInputs = [ pkgs.pkg-config ];
              };
            };
          };

          "pattern-discord" = {
            imports = [ globalCrateConfig ];
            autoWire = [ "crate" "clippy" ];
            path = ./../../crates/pattern_discord;
            crane = {
              args = {
                buildInputs =
                  commonBuildInputs
                  ++ [
                    pkgs.openssl
                    pkgs.pkg-config
                  ];
                nativeBuildInputs = [ pkgs.pkg-config ];
              };
            };
          };
        };
      };

      # Define the default package
      packages.default = self'.packages.pattern-cli;
      # packages.pattern = self'.packages.pattern-main;
      # packages.pattern-core = self'.packages.pattern-core;
      # packages.pattern-discord = self'.packages.pattern-discord;
      # packages.pattern-mcp = self'.packages.pattern-mcp;
      # packages.pattern-nd = self'.packages.pattern-nd;
    };
}
