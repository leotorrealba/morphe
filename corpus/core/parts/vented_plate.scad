// Real part: a vented cover plate — a grid of hex-ish vent slots punched
// through a panel by a nested for-loop.
// Constructs: difference, nested for-loop, translate, list comprehension, cube, cylinder.
panel = [60, 40, 3];
cols = 6;
rows = 4;
margin = 6;

step_x = (panel[0] - 2 * margin) / (cols - 1);
step_y = (panel[1] - 2 * margin) / (rows - 1);

difference() {
    cube(panel);
    for (i = [0 : cols - 1]) {
        for (j = [0 : rows - 1]) {
            translate([margin + i * step_x, margin + j * step_y, -1])
                cylinder(h = panel[2] + 2, r = 2.5, $fn = 6);
        }
    }
    // Corner mounting bores.
    for (c = [[4, 4], [panel[0] - 4, 4], [4, panel[1] - 4], [panel[0] - 4, panel[1] - 4]]) {
        translate([c[0], c[1], -1]) cylinder(h = panel[2] + 2, r = 1.8, $fn = 20);
    }
}
