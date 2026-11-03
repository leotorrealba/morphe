// Construct: string ops — str()/concat()/len() derive a dimension.
label = str("part-", 7);
tags = concat(["a", "b"], ["c", "d"]);
cube([len(label), len(tags) + 2, 4]);
