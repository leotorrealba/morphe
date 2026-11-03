// surface(): a .dat heightfield imports as a solid — relative paths resolve
// against this file's directory at eval time (upstream lookup_file
// semantics). 4x3 positive grid.
surface(file = "data/heightmap.dat");
