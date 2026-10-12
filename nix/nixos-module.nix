# The NixOS module, as `nixosModules.default` of this flake.
self:
{ config, lib, pkgs, ... }:

let
  cfg = config.services.jaunt;
  toml = pkgs.formats.toml { };
  configFile = toml.generate "jaunt-config.toml" cfg.settings;
  configured = cfg.settings != { };
in
{
  options.services.jaunt = {
    enable = lib.mkEnableOption ''
      jaunt, run-or-raise window switching for Hyprland.

      The daemon runs as a user service started by `graphical-session.target`,
      so the session has to reach systemd: `programs.hyprland.withUWSM` does
      that, as does anything else that imports the Wayland environment into
      the user manager
    '';

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "jaunt.packages.\${system}.default";
      description = "The Jaunt package to run.";
    };

    settings = lib.mkOption {
      type = toml.type;
      default = { };
      example = lib.literalExpression ''
        {
          keys = {
            next = "Tab";
            previous = "SHIFT + Tab";
            apps = {
              t = "ghostty";
              i = {
                cmd = "brave";
                app_id = "brave-browser";
              };
            };
          };
          switcher.delay = 90;
        }
      '';
      description = ''
        Jaunt's configuration, written to the Nix store and given to the
        daemon with `--config`.

        Left empty, the daemon reads each user's own
        `$XDG_CONFIG_HOME/jaunt/config.toml` instead, which is the one to
        use if you want to edit it without rebuilding.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    systemd.user.services.jaunt = {
      description = "Run-or-raise window switching for Hyprland";
      documentation = [ "https://github.com/mawkler/jaunt" ];

      # The switcher is only useful with a compositor, and goes when it does.
      bindsTo = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      wantedBy = [ "graphical-session.target" ];

      # The switcher is drawn by Quickshell, which the daemon looks for on
      # PATH. It comes from the system's own packages rather than Jaunt's:
      # anything that draws has to match the system's graphics drivers. The
      # profiles are what the applications Jaunt starts are installed in.
      path = [
        pkgs.quickshell
        "/run/wrappers"
        "%h/.nix-profile"
        "/etc/profiles/per-user/%u"
        "/run/current-system/sw"
      ];

      serviceConfig = {
        ExecStart = lib.concatStringsSep " " (
          [ "${cfg.package}/bin/jaunt" "daemon" ]
          ++ lib.optionals configured [ "--config" "${configFile}" ]
        );
        Restart = "on-failure";
        RestartSec = 2;
        Slice = "session.slice";
      };
    };
  };
}
