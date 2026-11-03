// surface(): centered variant — ORACLE-COMPARABLE. A center + invert variant
// was measured against OpenSCAD 2021.01: volume delta 0.60, hausdorff 1.077 —
// a DOCUMENTED divergence (morphe applies invert to .dat heightfields;
// OpenSCAD 2021.01 applies invert to PNG only), far past any tolerance. So
// invert is pinned analytically (surface_invert_flips_signs in the kernel),
// and this corpus file exercises center=true only.
surface(file = "data/heightmap.dat", center = true);
