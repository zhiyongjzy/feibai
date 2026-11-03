import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class SyncUserDictTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "tools").mkdir()
        (self.root / "data/dicts").mkdir(parents=True)
        shutil.copyfile(
            Path(__file__).parent.parent / "data/pinyin_syllables.txt",
            self.root / "data/pinyin_syllables.txt",
        )
        self.script = self.root / "tools/sync_user_dict.sh"
        shutil.copyfile(Path(__file__).with_name("sync_user_dict.sh"), self.script)
        for name in ("base", "tech", "extra"):
            (self.root / f"data/dicts/feibai.{name}.dict.yaml").write_text(
                "---\nname: test\n...\n", encoding="utf-8"
            )
        self.extra = self.root / "data/dicts/feibai.extra.dict.yaml"
        self.user = self.root / "user.dict.txt"
        self.user.write_text(
            "jingzhen\t晶振\tc=6 t=2965\n"
            "cu lue\t粗略\tc=3 t=2965\n"
            "haijidei\t海脊得\tc=8 t=2965\n"
            "gongzuomulu\t工作目录\tc=2 t=2965\n"
            "wangzong\t王总\tc=9 t=2965\n"
            "junk\t手abc\tc=9 t=2965\n",
            encoding="utf-8",
        )
        self.env = {
            **os.environ,
            "FEIBAI_USER_DICT": str(self.user),
            "FEIBAI_SYNC_EXCLUDE": str(self.root / "exclude.txt"),
        }
        self.original = self.extra.read_bytes()

    def run_sync(self, *args):
        return subprocess.run(
            ["bash", str(self.script), *args], env=self.env,
            capture_output=True, text=True, timeout=10,
        )

    def approve(self, text):
        path = self.root / "approved.tsv"
        path.write_text(text, encoding="utf-8")
        return str(path)

    def test_default_previews_every_candidate_without_writing(self):
        result = self.run_sync()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("晶振\tjingzhen\t6\t", result.stdout)
        self.assertIn("海脊得\thaijidei\t8\t", result.stdout)
        self.assertEqual(self.extra.read_bytes(), self.original)

    def test_write_requires_approval(self):
        result = self.run_sync("--write")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.extra.read_bytes(), self.original)

    def test_only_approved_words_are_written_and_rerun_is_idempotent(self):
        approved = self.approve("晶振\tjingzhen\n")
        for _ in range(2):
            result = self.run_sync("--write", "--approved", approved)
            self.assertEqual(result.returncode, 0, result.stderr)
        content = self.extra.read_text(encoding="utf-8")
        self.assertEqual(content.count("晶振\t"), 1)
        self.assertNotIn("海脊得", content)

    def test_approval_without_write_still_only_previews(self):
        result = self.run_sync("--approved", self.approve("晶振\tjingzhen\n"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.extra.read_bytes(), self.original)

    def test_alias_approval_uses_canonical_pinyin(self):
        result = self.run_sync("--write", "--approved", self.approve("粗略\tcu lue\n"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("粗略\tculve\t", self.extra.read_text(encoding="utf-8"))

    def test_syllable_boundaries_are_not_rewritten(self):
        self.user.write_text("nu er\t努尔\tc=3 t=2965\n", encoding="utf-8")
        result = self.run_sync("--write", "--approved", self.approve("努尔\tnu er\n"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("努尔\tnuer\t", self.extra.read_text(encoding="utf-8"))

    def test_compact_legacy_aliases_accept_spaced_approval(self):
        self.user.write_text(
            "culue\t粗略\tc=3 t=2965\nnuedai\t虐待\tc=3 t=2965\n", encoding="utf-8"
        )
        result = self.run_sync(
            "--write", "--approved", self.approve("粗略\tcu lve\n虐待\tnue dai\n")
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        content = self.extra.read_text(encoding="utf-8")
        self.assertIn("粗略\tculve\t", content)
        self.assertIn("虐待\tnvedai\t", content)

    def test_compact_boundaries_and_old_garbage_are_not_rewritten(self):
        self.user.write_text(
            "nuer\t努尔\tc=3 t=2965\nculue\t粗路鄂\tc=3 t=2965\n", encoding="utf-8"
        )
        result = self.run_sync()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("努尔\tnuer\t", result.stdout)
        self.assertIn("粗路鄂\tculue\t", result.stdout)
        self.assertNotIn("粗路鄂\tculve\t", result.stdout)

    def test_invalid_or_filtered_approval_fails_without_partial_write(self):
        for word, pinyin in [("晶振", "wrong"), ("王总", "wangzong"), ("不存在", "bucunzai"),
                             ("手abc", "junk"), ("工作目录", "gongzuomulu")]:
            with self.subTest(word=word, pinyin=pinyin):
                result = self.run_sync(
                    "--write", "--approved", self.approve(f"晶振\tjingzhen\n{word}\t{pinyin}\n")
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.extra.read_bytes(), self.original)

    def test_low_frequency_words_need_explicit_threshold(self):
        result = self.run_sync(
            "--write", "--min-count=1", "--approved",
            self.approve("工作目录\tgongzuomulu\n"),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("工作目录\t", self.extra.read_text(encoding="utf-8"))

    def test_exclusion_is_not_bypassed_by_approval(self):
        (self.root / "exclude.txt").write_text("晶振\n", encoding="utf-8")
        result = self.run_sync("--write", "--approved", self.approve("晶振\tjingzhen\n"))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.extra.read_bytes(), self.original)

    def test_known_words_are_not_duplicated(self):
        (self.root / "data/dicts/feibai.base.dict.yaml").write_text(
            "---\nname: base\n...\n晶振\tjing zhen\t100\n", encoding="utf-8"
        )
        result = self.run_sync("--write", "--approved", self.approve("晶振\tjingzhen\n"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.extra.read_bytes(), self.original)

    def test_invalid_arguments_are_reported(self):
        for args in [("--unknown",), ("--min-count",), ("--min-count=-1",),
                     ("--min-count=x",), ("--approved",)]:
            with self.subTest(args=args):
                result = self.run_sync(*args)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.extra.read_bytes(), self.original)


if __name__ == "__main__":
    unittest.main()
