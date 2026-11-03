// Construct: function definition + call feeding a dimension.
function box_height(base) = base * 2 + 1;
b = 5;
cube([b, b, box_height(b)]);
