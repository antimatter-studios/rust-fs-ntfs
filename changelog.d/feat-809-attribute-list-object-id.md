### Added

- **An object id set on a file that has an `$ATTRIBUTE_LIST`, or whose base
  record has no room for it, goes through the list (#809, part of #665).** It
  was refused with "the record carries an $ATTRIBUTE_LIST" or "exceeds record
  capacity". The `$OBJECT_ID` now stays where it is while its record holds it
  and goes to an extension record, with its list entry, when it does not;
  removing it takes the entry out. The `$Extend\$ObjId:$O` entry follows as
  before.
