### Fixed
- Applying a saved repair plan refuses a target that cannot flush its writes to stable storage before any write or flush, including an empty plan, and names the ordering requirement (#518, #1253).
