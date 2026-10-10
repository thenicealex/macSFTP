import pathlib
import tempfile
import unittest

from check_unsafe_boundary import check


class UnsafeBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='macsftp-unsafe-boundary-')
        self.addCleanup(self.directory.cleanup)
        self.root = pathlib.Path(self.directory.name)
        (self.root / 'Cargo.toml').write_text('[workspace]\nmembers = ["crates/native-menu", "crates/ui"]\n[workspace.lints.rust]\nunsafe_code = "forbid"\n')
        self.sources = self.root / 'crates/native-menu/src'
        self.sources.mkdir(parents=True)
        (self.sources.parent / 'Cargo.toml').write_text('[lib]\npath = "src/native_menu.rs"\n[lints.rust]\nunsafe_code = "deny"\n')
        ui = self.root / 'crates/ui'
        ui.mkdir()
        (ui / 'Cargo.toml').write_text('[lints]\nworkspace = true\n')
        (self.sources / 'native_menu.rs').write_text('#[cfg(target_os = "macos")]\n#[allow(unsafe_code)]\nmod macos;\n')
        (self.sources / 'macos.rs').write_text('unsafe fn approved_ffi() {}\n')

    def test_only_canonical_macos_file_allows_unsafe(self):
        self.assertEqual(check(self.root), [])
        nested = self.sources / 'nested'
        nested.mkdir()
        for code in ['unsafe {}', 'unsafe fn hidden() {}', 'unsafe impl Send for State {}', 'unsafe extern "C" {}', '#[unsafe(no_mangle)] fn exported() {}']:
            with self.subTest(code=code):
                (nested / 'macos.rs').write_text(code)
                self.assertTrue(check(self.root))

    def test_lint_and_external_module_escape_routes_are_rejected(self):
        source = self.sources / 'extra.rs'
        for code in ['#![allow(unsafe_code)]', '#[cfg_attr(test, allow(unsafe_code))] fn bypass() {}', 'include!("../../ffi.rs");', '#[path = "../../ffi.rs"] mod bypass;', '#[cfg_attr(test, path = "../../ffi.rs")] mod bypass;']:
            with self.subTest(code=code):
                source.write_text(code)
                self.assertTrue(check(self.root))

    def test_symlinked_module_directory_is_rejected(self):
        external = self.root / 'external'
        external.mkdir()
        (external / 'module.rs').write_text('unsafe fn escaped() {}')
        (self.sources / 'nested').symlink_to(external, target_is_directory=True)
        self.assertTrue(check(self.root))

    def test_comments_and_literals_do_not_create_false_positives(self):
        (self.sources / 'safe.rs').write_text('''// unsafe fn is prohibited
/* outer /* unsafe {} */ unsafe impl Send */
const DESCRIPTION: &str = "unsafe_code unsafe fn";
const RAW: &str = r##"unsafe {} \\" # unsafe extern"##;
''')
        self.assertEqual(check(self.root), [])

    def test_weakening_other_crates_or_the_gate_is_rejected(self):
        ui = self.root / 'crates/ui/Cargo.toml'
        ui.write_text('[lints.rust]\nunsafe_code = "allow"\n')
        self.assertTrue(check(self.root))
        ui.write_text('[lints]\nworkspace = true\n')
        library = self.sources / 'native_menu.rs'
        library.write_text('#[allow(unsafe_code)]\nmod macos;\n')
        self.assertTrue(check(self.root))


if __name__ == '__main__':
    unittest.main()
