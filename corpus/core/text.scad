// text(): glyph outlines from the bundled Fira Sans (OFL 1.1) as a 2D
// profile, lifted by linear_extrude into a solid.
// The font is PINNED to the bundled family: OpenSCAD's fontconfig default
// (Liberation Sans) differs from the bundled OFL family, so the oracle
// comparison requires the bundled font installed for fontconfig (CI skips it
// when openscad is absent).
linear_extrude(height = 5) text("hi", font = "Fira Sans");
