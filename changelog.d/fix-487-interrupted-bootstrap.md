### Fixed

- **Windows interrupted-write fixtures contain no formatter bootstrap code (#619).** All 26 partitions retain their BPBs, boot signatures, filesystem state and Windows recovery expectations. Primary and backup boot code and `$Boot` sectors 1–15 are zeroed, and the capture builder scrubs both captured and recovered VHDs before publication. CI checks every registered partition and tests the scrub's preservation and refusal behavior. Other fixture families and builders remain tracked by #487.
