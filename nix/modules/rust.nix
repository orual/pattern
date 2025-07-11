{ inputs, ... }: {
  imports = [
    inputs.rust-flake.flakeModules.default
    inputs.rust-flake.flakeModules.nixpkgs
  ];
  perSystem =
    { config
    , self'
    , pkgs
    , lib
    , ...
    }: {
      rust-project.crates."pattern".crane.args = {
        buildInputs = lib.optionals pkgs.stdenv.isDarwin (
          with pkgs.darwin.apple_sdk.frameworks; [
            IOKit
          ]
        );
      };
      packages.default = self'.packages.pattern;
    };
}
