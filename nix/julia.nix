{
  autoPatchelfHook,
  fetchurl,
  lib,
  stdenv,
  pin,
}:

stdenv.mkDerivation {
  pname = "julia-bin";
  inherit (pin) version;

  src = fetchurl {
    inherit (pin) url sha256;
  };

  nativeBuildInputs = [
    autoPatchelfHook
    stdenv.cc.cc
  ];

  dontConfigure = true;
  dontBuild = true;
  dontStrip = true;
  dontAutoPatchelf = true;

  installPhase = ''
    runHook preInstall
    cp -r . "$out"
    # Package images under share contain embedded checksums and must stay intact.
    autoPatchelf "$out/bin" "$out/lib" "$out/libexec"
    runHook postInstall
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    "$out/bin/julia" --startup-file=no ${./julia-smoke.jl} \
      ${lib.escapeShellArg pin.version} ${lib.escapeShellArg pin.commit}
    runHook postInstallCheck
  '';

  meta = {
    description = "Julia development build pinned with the parser oracle";
    homepage = "https://julialang.org";
    license = lib.licenses.gpl2Plus;
    mainProgram = "julia";
    platforms = [ "x86_64-linux" ];
  };
}
