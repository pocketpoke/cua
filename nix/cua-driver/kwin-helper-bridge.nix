# Builds the opt-in Plasma 6 / KWin session-bus publication bridge.
#
# The bridge is deliberately separate from the main cua-driver binary: the
# KWin declarative script is not loaded by default, and this package only
# exists for hosts that intentionally install the target-addressable helper.
{
  pkgs,
  src,
  ...
}:

pkgs.rustPlatform.buildRustPackage {
  pname = "kwin-helper-bridge";
  version = (pkgs.lib.importTOML "${src}/Cargo.toml").workspace.package.version;

  inherit src;

  cargoLock.lockFile = "${src}/Cargo.lock";
  cargoBuildFlags = [
    "-p"
    "platform-linux"
    "--features"
    "kwin-helper-bridge"
    "--bin"
    "kwin-helper-bridge"
  ];

  nativeBuildInputs = with pkgs; [
    pkg-config
    rustPlatform.bindgenHook
  ];
  buildInputs = with pkgs; [
    libx11
    libxi
    libxtst
    pipewire
    libei
    libxkbcommon
  ];

  doCheck = false;

  installPhase = ''
    runHook preInstall
    bridge_bin="$(find target -type f -path '*/release/kwin-helper-bridge' -print -quit)"
    test -n "$bridge_bin"
    install -Dm755 "$bridge_bin" "$out/bin/kwin-helper-bridge"
    runHook postInstall
  '';

  meta = with pkgs.lib; {
    description = "Experimental KWin target-input publication bridge for cua-driver";
    homepage = "https://github.com/trycua/cua";
    license = licenses.mit;
    mainProgram = "kwin-helper-bridge";
    platforms = platforms.linux;
  };
}