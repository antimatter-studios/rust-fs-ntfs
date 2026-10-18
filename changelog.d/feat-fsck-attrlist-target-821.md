`fsck::check_io` reports each `$ATTRIBUTE_LIST` entry whose named record
holds no attribute matching its type, exact name, lowest VCN and instance.
The finding names the base record, entry offset, target record and expected
attribute identity, without writing to the volume (#821, part of #667).
