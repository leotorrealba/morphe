// echo(...) and assert(...) as expressions: each performs its step, then
// yields the expression after it.
function checked_size(s) = assert(s > 0, "size must be positive") echo(size = s) s;

side = let(base = 3) checked_size(base * 2);
cube(side);
