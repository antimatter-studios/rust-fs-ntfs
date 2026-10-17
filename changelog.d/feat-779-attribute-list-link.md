### Added

- **A hard link whose `$FILE_NAME` the base record cannot hold is added
  through the file's `$ATTRIBUTE_LIST` (#779, part of #662).** `link` used to
  put the new name in the base record alone, so it was refused once that
  record was full or carried a list. The name now goes wherever the list
  puts it, base or extension record, as Windows lays names out, with its
  list entry; a file with no list whose base record is full is given one.
