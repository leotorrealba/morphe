// projection(cut=false) — the shadow: union of all cross-sections of a solid
// below origin, lifted back to a solid by linear_extrude so the corpus test
// classifies it Solid.
linear_extrude(height = 2) projection() translate([0, 0, -5]) cylinder(h = 10, r = 4);
