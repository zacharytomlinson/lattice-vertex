"""Surface compatibility boundaries for the package signer; no device access."""
import unittest

from update_package import surface_definition


class SurfaceDefinitionTests(unittest.TestCase):
    def test_metadata_independence(self):
        canonical = b"LSF\x03\x40\0" + bytes(8)
        for name in [b"", b"Light", "Dining room — light".encode()]:
            with self.subTest(name=name):
                exported = b"LSF\x04" + canonical[4:] + name + len(name).to_bytes(2, "little")
                self.assertEqual(surface_definition(exported), canonical)
        self.assertEqual(surface_definition(canonical), canonical)
        legacy = b"LSF\x02\x40" + bytes(8)
        self.assertEqual(surface_definition(legacy), legacy)

    def test_metadata_bounds(self):
        prefix = b"LSF\x04\x40\0" + bytes(8)
        for invalid in [
            b"", prefix, prefix + b"\x01", prefix + b"\xff\xff",
            prefix + b"\xff\x01\0", prefix + b"\0\x01\0",
            prefix + b"n" * 65520 + (65520).to_bytes(2, "little"),
        ]:
            with self.subTest(length=len(invalid)):
                with self.assertRaises(ValueError):
                    surface_definition(invalid)
        name = b"n" * 65519
        self.assertEqual(surface_definition(prefix + name + len(name).to_bytes(2, "little")),
                         b"LSF\x03" + prefix[4:])


if __name__ == "__main__":
    unittest.main()
