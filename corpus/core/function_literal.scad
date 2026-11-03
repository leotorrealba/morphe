// Function literals: functions as values, closures over their defining
// scope, and higher-order calls.
function scaler(k) = function(v) v * k;
double = scaler(2);
apply = function(f, x) f(x);
sizes = [for (s = [1, 2, 3]) double(s)];

assert(is_function(double));
assert((function(x) x + 1)(2) == 3);

for (i = [0 : len(sizes) - 1])
    translate([i * 10, 0, 0]) cube(apply(function(x) x + 1, sizes[i]));
