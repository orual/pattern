{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    flake-parts.inputs.nixpkgs-lib.follows = "nixpkgs";
    systems.url = "github:nix-systems/default";
    rust-flake.url = "github:juspay/rust-flake";
    rust-flake.inputs.nixpkgs.follows = "nixpkgs";
    process-compose-flake.url = "github:Platonic-Systems/process-compose-flake";

    # Tidepool runtime: provides the `tidepool-extract` GHC plugin binary
    # required by `pattern_runtime` to compile agent Haskell programs. Pinned
    # via flake.lock; bump with `nix flake update tidepool` when chasing
    # upstream API changes. When iterating against local tidepool changes,
    # use `nix develop --override-input tidepool path:../tidepool`.
    #
    # Currently pointed at our fork (orual/tidepool). Tracks upstream main
    # with Pattern-needed fixes (multi-module DataCon tag mismatch; planned
    # external-cancellation) applied on top. Fixes have pending upstream PRs;
    # swap back to `tidepool-heavy-industries/tidepool` once they merge.
    tidepool.url = "github:orual/tidepool";

    git-hooks.url = "github:cachix/git-hooks.nix";
    git-hooks.flake = false;
  };

  outputs = inputs:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      systems = import inputs.systems;

      # See ./nix/modules/*.nix for the modules that are imported here.
      imports = with builtins;
        map
          (fn: ./nix/modules/${fn})
          (attrNames (readDir ./nix/modules));
    };
}
