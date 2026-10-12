{
  description = "Run-or-raise for Hyprland";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    naersk.url = "github:nix-community/naersk/master";
    naersk.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      self,
      nixpkgs,
      naersk,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "x86_64-darwin"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;

      # The daemon runs on GLib's main loop.
      buildInputs = pkgs: [ pkgs.glib ];
    in
    {
      packages = forAllSystems (system: {
        default =
          let
            pkgs = import nixpkgs { inherit system; };
            naersk-lib = pkgs.callPackage naersk { };
          in
          naersk-lib.buildPackage {
            src = ./.;
            meta.mainProgram = "jaunt";
            nativeBuildInputs = with pkgs; [ pkg-config ];
            buildInputs = buildInputs pkgs;
          };
      });

      nixosModules = {
        jaunt = import ./nix/nixos-module.nix self;
        default = self.nixosModules.jaunt;
      };

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/jaunt";
        };
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.mkShell {
            nativeBuildInputs = with pkgs; [ pkg-config ];
            buildInputs =
              (with pkgs; [
                cargo
                rustc
                rustfmt
                rust-analyzer
                pre-commit
                rustPackages.clippy
              ])
              ++ buildInputs pkgs;
            RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
          };
        }
      );
    };
}
