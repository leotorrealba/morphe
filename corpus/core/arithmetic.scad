// Construct: arithmetic expressions (+ - * / %) computing dimensions.
base = 4;
side = base * 3 - 2;
gap = 20 % 7;
cube([side, side / 2, base + gap]);
