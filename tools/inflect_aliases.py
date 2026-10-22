"""Kind-classified inflection table from the Hunspell set already on disk.

data/spell/en_US.aff + en_US.dic hold the paradigm facts, but they mix
inflection (walked -> walk) with derivation (happiness -> happy) and never state
which relation a form has. Loading that table as-is would break the library's
contract that a derived word is never reported as an original form, so this tool
keeps only the relations the word library admits:

  * rule classes are limited to SFX D/G/R/S/T; B/H/J/L/M/N/P/V/X/Y/Z are
    derivational or possessive and are dropped;
  * every relation is labelled, with plural versus third-person singular resolved
    from the part of speech the library itself gives the lemma;
  * a rule-generated form must also occur in the library's own English text,
    because these affix rules over-generate ("runing", "stoped") and Hunspell
    records the real irregular and doubled forms separately instead. A rule -er
    form must further occur before "than", because Hunspell marks agentive words
    like "air" and "default" as gradable and the library's part-of-speech fields
    are too damaged by the source to overrule them.

Usage: python tools/inflect_aliases.py [--db store/dict.sqlite]
       [--out data/inflect_aliases.jsonl] [--stats]
"""

import argparse
import collections
import json
import re
import sqlite3
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
AFF = ROOT / "data/spell/en_US.aff"
DIC = ROOT / "data/spell/en_US.dic"

MAX_FORM_CHARS = 64

# The inflectional rule classes of en_US.aff. S covers the plural and the third-
# person singular at once; both readings are resolved per lemma below.
RULE_CLASSES = {
    "D": ("past", "past_participle"),
    "G": ("present_participle",),
    "R": ("comparative",),
    "T": ("superlative",),
    "S": ("plural", "third_person"),
}
VERB = {"verb", "transitiveverb", "intransitiveverb", "ambitransitiveverb", "auxiliary"}
NOUN = {"noun"}
PLURAL_OR_THIRD_PERSON = ("plural", "third_person")

# Spelling change from lemma to surface form, as recorded by an en_US.dic `st:`
# entry. Hunspell stores irregular and doubled forms as their own dictionary line
# with the true lemma attached, e.g. "running st:run" or "children st:child". An
# unlisted change is rejected rather than guessed at, because `st:` also records
# non-inflectional stems ("queen st:king").
SPELLING_CHANGES = {
    ("", "s"): PLURAL_OR_THIRD_PERSON,
    ("", "es"): PLURAL_OR_THIRD_PERSON,
    ("e", "s"): PLURAL_OR_THIRD_PERSON,
    ("e", "es"): PLURAL_OR_THIRD_PERSON,
    ("y", "ies"): PLURAL_OR_THIRD_PERSON,
    ("o", "es"): PLURAL_OR_THIRD_PERSON,
    ("", "ed"): ("past", "past_participle"),
    ("", "d"): ("past", "past_participle"),
    ("e", "d"): ("past", "past_participle"),
    ("y", "ied"): ("past", "past_participle"),
    ("", "ing"): ("present_participle",),
    ("e", "ing"): ("present_participle",),
    ("ie", "ying"): ("present_participle",),
    ("", "er"): ("comparative",),
    ("", "r"): ("comparative",),
    ("e", "r"): ("comparative",),
    ("y", "ier"): ("comparative",),
    ("", "est"): ("superlative",),
    ("", "st"): ("superlative",),
    ("e", "st"): ("superlative",),
    ("y", "iest"): ("superlative",),
    ("an", "en"): ("plural",),
    ("is", "es"): ("plural",),
    ("us", "i"): ("plural",),
    ("um", "a"): ("plural",),
    ("on", "a"): ("plural",),
    ("a", "ae"): ("plural",),
    ("ex", "ices"): ("plural",),
    ("ix", "iges"): ("plural",),
    ("f", "ves"): ("plural",),
    ("fe", "ves"): ("plural",),
    ("", "n"): ("past_participle",),
    ("", "en"): ("past_participle",),
}

# Suppletive forms share no spelling with their lemma, so no change matches.
# This is the closed set of irregular relations worth stating explicitly; only
# these exact pairs are accepted, never a generalised spelling change, which
# would also match "bell" -> "bold".
SUPPLETIVES = {
    ("be", "was"): ("past",),
    ("be", "were"): ("past",),
    ("be", "been"): ("past_participle",),
    ("be", "being"): ("present_participle",),
    ("go", "went"): ("past",),
    ("go", "gone"): ("past_participle",),
    ("good", "better"): ("comparative",),
    ("good", "best"): ("superlative",),
    ("well", "better"): ("comparative",),
    ("well", "best"): ("superlative",),
    ("bad", "worse"): ("comparative",),
    ("ill", "worse"): ("comparative",),
    ("bad", "worst"): ("superlative",),
    ("ill", "worst"): ("superlative",),
    ("old", "elder"): ("comparative",),
    ("old", "eldest"): ("superlative",),
    ("far", "further"): ("comparative",),
    ("far", "farther"): ("comparative",),
    ("far", "furthest"): ("superlative",),
    ("far", "farthest"): ("superlative",),
    ("little", "less"): ("comparative",),
    ("little", "least"): ("superlative",),
    ("much", "more"): ("comparative",),
    ("many", "more"): ("comparative",),
    ("much", "most"): ("superlative",),
    ("many", "most"): ("superlative",),
    ("do", "did"): ("past",),
    ("do", "done"): ("past_participle",),
    ("do", "does"): PLURAL_OR_THIRD_PERSON,
    ("have", "had"): ("past", "past_participle"),
    ("shall", "should"): ("past",),
    ("will", "would"): ("past",),
    ("can", "could"): ("past",),
    ("may", "might"): ("past",),
}

FORM_SHAPE = re.compile(r"^[a-z][a-z'.\- ]*$")

# The remaining `st:` spellings, read off the dictionary itself rather than from
# memory: every shape below is a family the file actually records, and the label
# follows the forms that family contains (ran with ring->rang/rung split past
# from participle, sold and held covering both). Feminine and spelling-variant
# stems (author->authoress, peony->paeony, king->queen) are left out on purpose,
# so they stay unclassified and are rejected as derivation.
PAST = ("past",)
PARTICIPLE = ("past_participle",)
PAST_BOTH = ("past", "past_participle")
PROGRESSIVE = ("present_participle",)
PLURALS = ("plural",)

IRREGULAR_SHAPES = {
    # -t/-d preterites, which serve as participle too
    ("ed", "d"): PAST_BOTH,
    ("ad", "d"): PAST_BOTH,
    ("d", "t"): PAST_BOTH,
    ("l", "t"): PAST_BOTH,
    ("el", "lt"): PAST_BOTH,
    ("ght", "t"): PAST_BOTH,
    ("et", "t"): PAST_BOTH,
    ("e", "t"): PAST_BOTH,
    ("", "t"): PAST_BOTH,
    ("ot", "t"): PAST_BOTH,
    ("ave", "ft"): PAST_BOTH,
    ("ep", "pt"): PAST_BOTH,
    ("and", "ood"): PAST_BOTH,
    ("ind", "ound"): PAST_BOTH,
    ("old", "eld"): PAST_BOTH,
    ("ell", "old"): PAST_BOTH,
    ("tch", "ught"): PAST_BOTH,
    ("ight", "ought"): PAST_BOTH,
    ("ing", "ought"): PAST_BOTH,
    ("ink", "ought"): PAST_BOTH,
    ("uy", "ought"): PAST_BOTH,
    ("each", "aught"): PAST_BOTH,
    ("eech", "ought"): PAST_BOTH,
    ("eek", "ought"): PAST_BOTH,
    ("ick", "uck"): PAST_BOTH,
    ("ike", "uck"): PAST_BOTH,
    ("ig", "ug"): PAST_BOTH,
    ("y", "id"): PAST_BOTH,
    ("ke", "de"): PAST_BOTH,
    ("go", "went"): PAST,
    # consonant-doubled spellings Hunspell cannot derive
    ("", "ped"): PAST_BOTH,
    ("", "ping"): PROGRESSIVE,
    ("", "ked"): PAST_BOTH,
    ("", "king"): PROGRESSIVE,
    ("f", "ved"): PAST_BOTH,
    ("f", "ving"): PROGRESSIVE,
    # preterite only
    ("ake", "ook"): PAST,
    ("eak", "oke"): PAST,
    ("eal", "ole"): PAST,
    ("ear", "ore"): PAST,
    ("eave", "ave"): PAST,
    ("eave", "ove"): PAST,
    ("ave", "ove"): PAST,
    ("eeze", "oze"): PAST,
    ("ee", "aw"): PAST,
    ("et", "ot"): PAST,
    ("ide", "ode"): PAST,
    ("ine", "one"): PAST,
    ("ise", "ose"): PAST,
    ("ite", "ote"): PAST,
    ("ive", "ave"): PAST,
    ("ive", "ove"): PAST,
    ("ose", "se"): PAST,
    ("ow", "ew"): PAST,
    ("aw", "ew"): PAST,
    ("ay", "ew"): PAST,
    ("y", "ew"): PAST,
    ("ome", "ame"): PAST,
    ("all", "ell"): PAST,
    ("ing", "ang"): PAST,
    ("ink", "ank"): PAST,
    ("im", "am"): PAST,
    ("in", "an"): PAST,
    ("in", "on"): PAST,
    ("un", "an"): PAST,
    ("it", "at"): PAST,
    ("ead", "od"): PAST,
    ("o", "id"): PAST,
    ("e", ""): PAST,
    ("ve", "s"): ("third_person",),
    # participle only
    ("e", "ten"): PARTICIPLE,
    ("e", "den"): PARTICIPLE,
    ("", "den"): PARTICIPLE,
    ("", "ne"): PARTICIPLE,
    ("ake", "oken"): PARTICIPLE,
    ("eak", "oken"): PARTICIPLE,
    ("eal", "olen"): PARTICIPLE,
    ("ear", "orn"): PARTICIPLE,
    ("ear", "orne"): PARTICIPLE,
    ("eave", "oven"): PARTICIPLE,
    ("ose", "sen"): PARTICIPLE,
    ("et", "otten"): PARTICIPLE,
    ("ead", "odden"): PARTICIPLE,
    ("ke", "cken"): PARTICIPLE,
    ("ing", "ung"): PARTICIPLE,
    ("ink", "unk"): PARTICIPLE,
    ("im", "um"): PARTICIPLE,
    ("in", "un"): PARTICIPLE,
    ("ie", "ain"): PARTICIPLE,
    ("y", "in"): PARTICIPLE,
    ("y", "own"): PARTICIPLE,
    # Latin, Greek and Germanic plurals
    ("", "e"): PLURALS,
    ("a", "e"): PLURALS,
    ("s", "des"): PLURALS,
    ("s", "ntes"): PLURALS,
    ("x", "ces"): PLURALS,
    ("x", "ges"): PLURALS,
    ("yx", "ix"): PLURALS,
    ("o", "i"): PLURALS,
    ("e", "i"): PLURALS,
    ("o", "ines"): PLURALS,
    ("", "x"): PLURALS,
    ("", "zes"): PLURALS,
    ("", "im"): PLURALS,
    ("", "ta"): PLURALS,
    ("us", "era"): PLURALS,
    ("us", "ora"): PLURALS,
    ("e", "ia"): PLURALS,
    ("ous", "a"): PLURALS,
    ("f", "ves"): PLURALS,
    ("ff", "ves"): PLURALS,
    ("ny", "ce"): PLURALS,
    ("oot", "eet"): PLURALS,
    ("ooth", "eeth"): PLURALS,
    ("oose", "eese"): PLURALS,
    ("ouse", "ice"): PLURALS,
    ("", "ren"): PLURALS,
    ("rson", "ople"): PLURALS,
    ("by", "sby"): PLURALS,
    ("adame", "esdames"): PLURALS,
    ("ademoiselle", "esdemoiselles"): PLURALS,
}


def doubled_spelling(lemma: str, strip: str, add: str):
    """The consonant-doubled spelling of the same ending, when the lemma has its shape.

    Which verbs double is a matter of stress rather than orthography ("plan" gives
    "planned" but "develop" gives "developed"), so this only proposes the rival
    spelling and lets the corpus decide which of the two the language actually uses.
    """
    if strip or len(lemma) < 3 or not add or add[0] not in "aeiou":
        return None
    if lemma[-1] in "wxhz" or not re.search(r"[^aeiou][aeiou][^aeiou]$", lemma):
        return None
    return (lemma + lemma[-1] + add).lower()


def parse_affix_rules(path: Path):
    """Only the enabled SFX blocks; PFX and commented-out blocks are ignored."""
    rules = collections.defaultdict(list)
    header = re.compile(r"^(SFX|PFX)\s+(\S)\s+[YN]\s+\d+$")
    entry = re.compile(r"^(SFX|PFX)\s+(\S)\s+(\S+)\s+(\S+)\s+(\S+)")
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        match = header.match(line)
        if match:
            rules[(match.group(1), match.group(2))]
            continue
        match = entry.match(line)
        if match and (match.group(1), match.group(2)) in rules:
            rules[(match.group(1), match.group(2))].append(
                (
                    "" if match.group(3) == "0" else match.group(3),
                    "" if match.group(4) == "0" else match.group(4),
                    match.group(5),
                )
            )
    return rules


def apply_rule(lemma: str, strip: str, add: str, condition: str):
    if strip and not lemma.endswith(strip):
        return None
    if condition and condition != ".":
        try:
            if re.search(condition + "$", lemma) is None:
                return None
        except re.error:
            return None
    return (lemma[: len(lemma) - len(strip)] if strip else lemma) + add


def read_entries(path: Path):
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines()[1:]:
        line = line.strip()
        if not line:
            continue
        parts = line.split()
        stem = next((p[3:] for p in parts[1:] if p.startswith("st:")), None)
        word, _, flags = parts[0].partition("/")
        if word:
            yield word, flags, stem


def spelling_change(lemma: str, form: str):
    shared = 0
    while shared < min(len(lemma), len(form)) and lemma[shared] == form[shared]:
        shared += 1
    return lemma[shared:], form[shared:]


# en_US.dic does not flag doubled forms through the affix rules; it lists them as
# their own line, e.g. "stopped st:stop". Read that as one repeated consonant plus
# an ordinary inflectional ending, and require the consonant-vowel-consonant shape
# so that "played" is never mistaken for a doubling of "play".
DOUBLED_ENDINGS = {
    "ed": ("past", "past_participle"),
    "d": ("past", "past_participle"),
    "ing": ("present_participle",),
    "er": ("comparative",),
    "est": ("superlative",),
}


def doubling_kinds(lemma: str, form: str):
    stem_tail, form_tail = spelling_change(lemma, form)
    if stem_tail or len(form_tail) < 3 or len(lemma) < 3:
        return ()
    repeated, ending = form_tail[0], form_tail[1:]
    if not lemma.endswith(repeated) or lemma[-2] not in "aeiou":
        return ()
    return DOUBLED_ENDINGS.get(ending, ())


def stem_kinds(lemma: str, form: str, pos: set[str]):
    pair = (lemma.lower(), form.lower())
    if pair in SUPPLETIVES:
        return resolve(SUPPLETIVES[pair], pos)
    change = spelling_change(lemma, form)
    kinds = SPELLING_CHANGES.get(change)
    if not kinds:
        kinds = doubling_kinds(lemma, form)
    if not kinds:
        kinds = IRREGULAR_SHAPES.get(change, ())
    return resolve(kinds, pos)


def resolve(kinds, pos: set[str]):
    """Narrow the classes whose two readings happen to share one spelling."""
    if not kinds:
        return ()
    if tuple(kinds) == PLURAL_OR_THIRD_PERSON:
        verb, noun = bool(pos & VERB), bool(pos & NOUN)
        if verb and not noun:
            return ("third_person",)
        if noun and not verb:
            return ("plural",)
        return PLURAL_OR_THIRD_PERSON
    return tuple(kinds)


def load_library(db_path: Path):
    """Lemma parts of speech plus the headword list the aliases table can point at."""
    conn = sqlite3.connect(db_path)
    pos_by_lemma = collections.defaultdict(set)
    keys = set()
    for key, blob in conn.execute("select key, card from entries"):
        card = json.loads(zlib.decompress(blob[4:]))
        keys.add(key)
        pos_by_lemma[key] |= set(card.get("pos") or [])
        for sense in card.get("senses") or []:
            if sense.get("pos"):
                pos_by_lemma[key].add(sense["pos"])
    conn.close()
    return keys, pos_by_lemma


def count_usage(path: Path, candidates: set[str]):
    """Corpus frequency per candidate form, plus how often it stands before "than".

    An affix rule firing is only a hypothesis: en_US.dic flags verbs whose real
    past needs a doubled consonant, so a bare rule pass also yields forms that
    belong to no paradigm at all. Frequency in the dictionary's own text is what
    separates a relation the dictionary intends from a rule that merely fires.
    The "than" column settles gradability, which the library's part-of-speech
    fields do not: en_US.dic marks "air" and "default" as taking -er, and the
    forms it produces are agentive nouns, not comparatives.
    """
    needle = {word.encode(): word for word in candidates}
    hits = collections.Counter()
    than = collections.Counter()
    found = re.compile(rb"[a-z]+")
    previous = None
    for match in found.finditer(path.read_bytes().lower()):
        word = needle.get(match.group())
        if word:
            hits[word] += 1
        if previous is not None and match.group() == b"than":
            graded = needle.get(previous)
            if graded:
                than[graded] += 1
        previous = match.group()
    return hits, than


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default=str(ROOT / "store/dict.sqlite"))
    ap.add_argument("--corpus", default=str(ROOT / "data/combined.inflated"))
    ap.add_argument("--out", default=str(ROOT / "data/inflect_aliases.jsonl"))
    ap.add_argument("--min-hits", type=int, default=2)
    ap.add_argument("--min-than", type=int, default=1)
    ap.add_argument("--stats", action="store_true")
    args = ap.parse_args()

    keys, pos_by_lemma = load_library(Path(args.db))
    rules = parse_affix_rules(AFF)
    entries = list(read_entries(DIC))

    # A rule that fires is not yet a word. en_US.dic marks verbs whose real past
    # or participle doubles a consonant only through a separate `st:` line, so the
    # bare expansion also yields shapes like "planed" for plan. Usage in the
    # dictionary's own text decides; `st:` lines need no such check.
    proposed = []
    for word, flags, stem in entries:
        lemma = stem or word
        pos = pos_by_lemma.get(lemma.lower(), set())
        if stem:
            proposed.append((word, lemma, stem_kinds(lemma, word, pos),
                             f"hunspell en_US.dic {word} st:{stem}", "stem", None))
        for flag in set(flags):
            if flag not in RULE_CLASSES:
                continue
            for strip, add, condition in rules.get(("SFX", flag), ()):
                generated = apply_rule(lemma, strip, add, condition)
                if generated:
                    proposed.append((generated, lemma, resolve(RULE_CLASSES[flag], pos),
                                     f"hunspell en_US.aff SFX {flag} {strip or 0}->{add}",
                                     "rule", doubled_spelling(lemma, strip, add)))

    need_usage = {p[0].strip().lower() for p in proposed if p[4] == "rule"}
    need_usage |= {p[5] for p in proposed if p[5]}
    if need_usage:
        hits, than = count_usage(Path(args.corpus), need_usage)
    else:
        hits = than = collections.Counter()

    # A gradable adjective is one the language actually compares: prove the lemma
    # with its -er form before accepting an -est form that shares that spelling.
    gradable = {
        lemma.strip().lower()
        for form, lemma, kinds, _, where, _ in proposed
        if where == "rule" and "comparative" in kinds and than[form.strip().lower()] >= args.min_than
    }

    rows = {}
    origin = collections.Counter()
    rejected = collections.Counter()
    for form, lemma, kinds, evidence, where, rival in proposed:
        form, lemma = form.strip().lower(), lemma.strip().lower()
        if not kinds:
            rejected["unclassified relation"] += 1
            continue
        if not form or form == lemma or len(form) > MAX_FORM_CHARS:
            rejected["identity or length"] += 1
            continue
        if not FORM_SHAPE.match(form):
            rejected["non-lexical form"] += 1
            continue
        if lemma not in keys:
            rejected["lemma absent from library"] += 1
            continue
        if where == "rule" and hits[form] < args.min_hits:
            rejected["rule output unused in corpus"] += 1
            continue
        if rival and hits[rival] > hits[form]:
            rejected["rule output crowded out by doubled spelling"] += 1
            continue
        if where == "rule" and "comparative" in kinds and than[form] < args.min_than:
            rejected["-er form never used before than"] += 1
            continue
        if where == "rule" and "superlative" in kinds and lemma not in gradable:
            rejected["-est form on a lemma never compared"] += 1
            continue
        for kind in kinds:
            key = (form, lemma, kind)
            if key not in rows:
                rows[key] = evidence
                origin[(where, kind)] += 1

    if args.stats:
        kinds = collections.Counter(kind for _, _, kind in rows)
        print(f"library keys: {len(keys)}  dic entries: {len(entries)}  proposals: {len(proposed)}")
        print(f"rows: {len(rows)}  distinct forms: {len({f for f, _, _ in rows})}")
        print(f"forms already a library key: {sum(1 for f, _, _ in rows if f in keys)}")
        print(f"kinds: {kinds.most_common()}")
        print(f"origin: {sorted((f'{w}/{k}', c) for (w, k), c in origin.items())}")
        print(f"rejected: {rejected.most_common()}")
        probes = ("running", "runs", "ran", "boxes", "watches", "studied", "better", "worker",
                  "children", "planed", "planned", "developed", "narrower", "defaulter",
                  "stoped", "runing", "happiness", "childs")
        for probe in probes:
            found = sorted({(l, k) for f, l, k in rows if f == probe})
            print(f"  {probe:11} hits={hits.get(probe, 0):>6} than={than.get(probe, 0):>4}  {found if found else '-'}")
        return 0

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w", encoding="utf-8") as handle:
        for form, lemma, kind in sorted(rows):
            handle.write(json.dumps(
                {"form": form, "lemma": lemma, "kind": kind, "evidence": rows[(form, lemma, kind)]},
                ensure_ascii=False) + "\n")
    print(f"wrote {len(rows)} rows -> {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
