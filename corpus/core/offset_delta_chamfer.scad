// offset() in delta+chamfer mode: beveled corners (Clipper2 square joins),
// lifted to a solid by linear_extrude as the corpus test requires a solid for
// non-_2d files.
linear_extrude(height = 3) offset(delta = 2, chamfer = true) square(10);
