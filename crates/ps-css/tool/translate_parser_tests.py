#!/usr/bin/env python3
"""Translate parser_unittest.cc TEST_F bodies to Rust unit tests.

Usage: translate_parser_tests.py <parser_unittest.cc> <out.rs>

Generates a `#[cfg(test)]` submodule (white-box access to parser
internals). Fails loudly on any statement or expression shape it
does not recognize. BoundsCopiedMediaQueries is hand-written in the
preamble (string building); everything else is mechanical.
"""

import re
import sys

# Tests whose committed expectations contradict the committed C++ code,
# proven by compiling and running the vendored parser directly (see
# DIVERGENCE notes on the hand-written versions in the preamble):
# - Unescape: predicate accepts 0xABCDEF, test expects a space.
# - ValueValidation, AcceptAllValues: no value validation exists;
#   the parser keeps every declaration (the passing minify goldens,
#   e.g. `width: 10`, confirm this).
# - AtFontFace: font-variant is kept, not dropped.
SKIP = {
    "BoundsCopiedMediaQueries",
    "unescape",
    "value_validation",
    "AcceptAllValues",
    "AtFontFace",
}

UNIT_ENUMS = {
    "EM": "Em", "EX": "Ex", "PX": "Px", "CM": "Cm", "MM": "Mm",
    "IN": "In", "PT": "Pt", "PC": "Pc", "DEG": "Deg", "RAD": "Rad",
    "GRAD": "Grad", "MS": "Ms", "S": "S", "HZ": "Hz", "KHZ": "Khz",
    "PERCENT": "Percent", "VH": "Vh", "VM": "Vm", "VW": "Vw",
    "OTHER": "Other", "NO_UNIT": "NoUnit",
}

VALUE_TYPES = {
    "NUMBER": "Number", "URI": "Uri", "FUNCTION": "Function",
    "RECT": "Rect", "COLOR": "Color", "STRING": "String",
    "IDENT": "Ident", "COMMA": "Comma", "UNKNOWN": "Unknown",
    "DEFAULT": "Default",
}

ERROR_FLAGS = {
    "kNoError": "NO_ERROR", "kUtf8Error": "UTF8_ERROR",
    "kDeclarationError": "DECLARATION_ERROR",
    "kSelectorError": "SELECTOR_ERROR", "kFunctionError": "FUNCTION_ERROR",
    "kMediaError": "MEDIA_ERROR", "kCounterError": "COUNTER_ERROR",
    "kHtmlCommentError": "HTML_COMMENT_ERROR",
    "kValueError": "VALUE_ERROR", "kRulesetError": "RULESET_ERROR",
    "kSkippedTokenError": "SKIPPED_TOKEN_ERROR",
    "kCharsetError": "CHARSET_ERROR", "kBlockError": "BLOCK_ERROR",
    "kNumberError": "NUMBER_ERROR", "kImportError": "IMPORT_ERROR",
    "kAtRuleError": "AT_RULE_ERROR", "kCssCommentError": "CSS_COMMENT_ERROR",
}

METHODS = {
    "GetLexicalUnitType": "lexical_type", "GetDimension": "dimension",
    "GetFloatValue": "float_value", "GetIntegerValue": "integer_value",
    "GetIdentifier": "identifier", "GetIdentifierText": "identifier_text",
    "GetStringValue": "string_value", "GetColorValue": "color_value",
    "GetFunctionName": "function_name", "GetParameters": "parameters",
    "GetParametersWithSeparators": "parameters_with_separators",
    "ToString": "to_css_string", "Done": "done",
    "ParseAny": "parse_any",
    "ParseAnyExpectingColor": "parse_any_expecting_color",
    "ParseColor": "parse_color",
    "ParseDeclarations": "parse_declarations",
    "ParseRawDeclarations": "parse_raw_declarations",
    "ParseFont": "parse_font", "ParseFontFamily": "parse_font_family",
    "ParseMediaQueries": "parse_media_queries",
    "ParseRuleset": "parse_ruleset",
    "ParseSelectors": "parse_selectors",
    "ParseSimpleSelectors": "parse_simple_selectors",
    "ParseStatement": "parse_statement",
    "ParseStylesheet": "parse_stylesheet",
    "ParseRawStylesheet": "parse_raw_stylesheet",
    "ParseValues": "parse_values", "ParseNumber": "parse_number",
    "SkipMatching": "skip_matching",
    "SkipPastDelimiter": "skip_past_delimiter",
    "SkipSpace": "skip_space",
    "SkipToAtRuleEnd": "skip_to_at_rule_end",
    "SkipToMediaQueryEnd": "skip_to_media_query_end",
    "SkipToNextAny": "skip_to_next_any",
    "ExtractCharset": "extract_charset",
    "ParseAsSingleImport": "parse_as_single_import",
    "ParseNextImport": "parse_next_import",
    "ParseCharset": "parse_charset",
    "errors_seen_mask": "errors_seen_mask",
    "unparseable_sections_seen_mask": "unparseable_sections_seen_mask",
    "set_quirks_mode": "set_quirks_mode",
    "set_preservation_mode": "set_preservation_mode",
    "set_max_function_depth": "set_max_function_depth",
    "reset": "RESET", "get": "GET", "at": "AT",
    "size": "len", "length": "len", "empty": "is_empty",
    "getpos": "GETPOS", "in_": "IN_",
    "type": "kind", "attribute": "attribute", "value": "value",
    "pseudoclass": "pseudoclass",
    "pseudoclass_separator": "pseudoclass_separator",
    "lang": "lang", "element_text": "element_text",
    "element_type": "element_type", "combinator": "combinator",
    "link": "link", "media_queries": "media_queries",
    "media_query": "media_query", "media_type": "media_type",
    "expressions": "expressions", "expression": "expression",
    "name": "name", "has_value": "has_value",
    "qualifier": "qualifier", "prop": "prop",
    "values": "values", "IsImportant": "is_important",
    "prop_text": "prop_text",
    "bytes_in_original_buffer": "bytes_in_original_buffer",
    "charset": "charset", "charsets": "charsets",
    "import": "import", "imports": "imports",
    "font_face": "font_face", "font_faces": "font_faces",
    "ruleset": "ruleset", "rulesets": "rulesets",
    "selector": "selector", "selectors": "selectors",
    "declaration": "declaration",
    "declarations": "declarations",
    "separator": "separator", "Equals": "equals",
    "ident": "ident", "ident_text": "ident_text",
    "rbegin": "RBEGIN", "IsDefined": "is_defined",
    "r": "r", "g": "g", "b": "b", "rgb": "rgb_value",
}

# Methods whose pointer-nullable C++ result is already Option in Rust.
# (Everything else nullable-but-infallible gets wrapped in Some().)
ALREADY_OPTION = {
    "ParseAndExpandBackground", "parse_and_expand_background",
    "parse_any", "parse_any_expecting_color", "parse_number",
    "parse_rgb_color", "parse_url", "parse_function",
    "parse_values", "parse_font", "parse_selectors",
    "parse_simple_selectors", "parse_ruleset", "parse_import",
    "parse_font_face", "parse_next_import", "parse_as_single_import",
    "parse_media_query", "parse_attribute_selector",
    "parse_simple_selector",
}

# Value-returning callees (never Option).
VALUE_CALLEES = {
    "parse_ident", "parse_escape", "parse_string", "parse_string_or_ident",
    "parse_color", "parse_charset", "extract_charset",
}


def camel(name):
    return "".join(p[:1].upper() + p[1:].lower() for p in name.split("_"))


def strip_comments(body):
    out, i, n = [], 0, len(body)
    while i < n:
        c = body[i]
        if c == '"':
            out.append(c)
            i += 1
            while i < n:
                out.append(body[i])
                if body[i] == "\\":
                    i += 1
                    if i < n:
                        out.append(body[i])
                    i += 1
                elif body[i] == '"':
                    i += 1
                    break
                else:
                    i += 1
        elif c == "'" and i + 2 < n and body[i + 2] == "'":
            out.append(body[i:i + 3])
            i += 3
        elif c == "/" and i + 1 < n and body[i + 1] == "/":
            while i < n and body[i] != "\n":
                i += 1
        elif c == "/" and i + 1 < n and body[i + 1] == "*":
            i += 2
            while i + 1 < n and not (body[i] == "*" and body[i + 1] == "/"):
                i += 1
            i += 2
        else:
            out.append(c)
            i += 1
    return "".join(out)


def split_top(body, delimiter):
    """Split on delimiter at depth 0, strings/chars respected."""
    parts, depth, cur, instr, inchar, esc = [], 0, [], False, False, False
    i = 0
    while i < len(body):
        c = body[i]
        if instr:
            cur.append(c)
            if esc:
                esc = False
            elif c == "\\":
                esc = True
            elif c == '"':
                instr = False
        elif inchar:
            cur.append(c)
            if esc:
                esc = False
            elif c == "\\":
                esc = True
            elif c == "'":
                inchar = False
        elif c == '"':
            instr = True
            cur.append(c)
        elif c == "'":
            inchar = True
            cur.append(c)
        elif c in "([{":
            depth += 1
            cur.append(c)
        elif c in ")]}":
            depth -= 1
            cur.append(c)
        elif c == delimiter and depth == 0:
            parts.append("".join(cur))
            cur = []
        else:
            cur.append(c)
        i += 1
    parts.append("".join(cur))
    return parts


CXX_ESCAPES = {
    "n": 10, "r": 13, "t": 9, "f": 12, "v": 11, "a": 7, "b": 8,
    "\\": 92, '"': 34, "?": 63, "'": 39, "0": 0,
}


def cxx_decode(literal):
    assert literal.startswith('"') and literal.endswith('"'), literal
    inner, out, i = literal[1:-1], bytearray(), 0
    while i < len(inner):
        c = inner[i]
        if c != "\\":
            # Raw source chars contribute their UTF-8 bytes.
            out.extend(c.encode("utf-8"))
            i += 1
            continue
        i += 1
        e = inner[i]
        if e == "x":
            i += 1
            digits = ""
            while i < len(inner) and inner[i] in "0123456789ABCDEFabcdef":
                digits += inner[i]
                i += 1
            assert len(digits) == 2, literal
            out.append(int(digits, 16))
        elif e in "01234567":
            digits = ""
            while i < len(inner) and len(digits) < 3 \
                    and inner[i] in "01234567":
                digits += inner[i]
                i += 1
            out.append(int(digits, 8) & 0xFF)
        else:
            assert e in CXX_ESCAPES, literal
            out.append(CXX_ESCAPES[e])
            i += 1
    return bytes(out)


def rust_bytes(data):
    parts = []
    for byte in data:
        if byte == 10:
            parts.append("\\n")
        elif byte == 13:
            parts.append("\\r")
        elif byte == 9:
            parts.append("\\t")
        elif byte == 34:
            parts.append('\\"')
        elif byte == 92:
            parts.append("\\\\")
        elif 32 <= byte < 127:
            parts.append(chr(byte))
        else:
            parts.append(f"\\x{byte:02x}")
    return 'b"' + "".join(parts) + '"'


def rust_char(literal):
    """Translate a C++ char literal to a Rust byte literal."""
    assert literal.startswith("'") and literal.endswith("'"), literal
    inner = literal[1:-1]
    if len(inner) == 1:
        byte = ord(inner)
    else:
        assert inner[0] == "\\", literal
        e = inner[1]
        if e == "x":
            byte = int(inner[2:], 16)
        elif e in CXX_ESCAPES:
            byte = CXX_ESCAPES[e]
        else:
            raise AssertionError(literal)
    if byte == 92:
        return "b'\\\\'"
    if byte == 39:
        return "b'\\''"
    if 32 <= byte < 127:
        return f"b'{chr(byte)}'"
    return f"{byte}u8"


class Translator:
    def __init__(self):
        self.symbols = {}

    def expr(self, text):
        """Translate a C++ expression to Rust."""
        text = text.strip()
        # String literal concatenation.
        if text.startswith('"'):
            lits = re.findall(r'"(?:[^"\\]|\\.)*"', text)
            rest = text
            for lit in lits:
                rest = rest.replace(lit, "", 1)
            assert rest.strip() == "", text
            return rust_bytes(b"".join(cxx_decode(l) for l in lits))
        # Char literal.
        if re.fullmatch(r"'(?:[^'\\]|\\.)+'", text):
            return rust_char(text)
        # Numbers, bools, NULL.
        if re.fullmatch(r"-?(\d+(\.\d+)?|\.\d+)([eE][+-]?\d+)?", text):
            if text.startswith("."):
                return "0" + text
            if text.startswith("-."):
                return "-0" + text[1:]
            return text
        if re.fullmatch(r"0[xX][0-9A-Fa-f]+", text):
            return "0x" + text[2:].lower()
        if text in ("true", "false"):
            return text
        if text == "NULL":
            return "None"
        # Casts.
        m = re.fullmatch(r"static_cast<char32>\((.*)\)", text, re.S)
        if m:
            return f"u32::from({self.expr(m.group(1))})"
        m = re.fullmatch(r"static_cast<.*\*>\(NULL\)", text)
        if m:
            return "None"
        # strlen.
        m = re.fullmatch(r"strlen\((.*)\)", text, re.S)
        if m:
            return f"{self.expr(m.group(1))}.len()"
        # UTF8ToUnicodeText.
        m = re.fullmatch(r"UTF8ToUnicodeText\((.*)\)", text, re.S)
        if m:
            return f"UnicodeText::from_utf8({self.expr(m.group(1))})"
        # UnicodeTextToUTF8.
        m = re.fullmatch(r"UnicodeTextToUTF8\((.*)\)", text, re.S)
        if m:
            return f"({self.expr(m.group(1))}).as_bytes()"
        # new-expressions.
        m = re.fullmatch(r"new Parser\((.*)\)", text, re.S)
        if m:
            args = split_top(m.group(1), ",")
            assert len(args) == 1, text
            return f"Parser::new({self.expr(args[0])})"
        if text == "new Values":
            return "Vec::new()"
        m = re.fullmatch(
            r"new (Stylesheet|MediaQueries|Ruleset|Declarations|Import|"
            r"FontFace|Values|Selector|SimpleSelectors)\(\)", text)
        if m:
            name = m.group(1)
            return "Vec::new()" if name == "Values" else f"{name}::new()"
        m = re.fullmatch(r"new Value\((.*)\)", text, re.S)
        if m:
            inner = m.group(1).strip()
            if inner.startswith("*"):
                inner = inner[1:]
            translated = self.expr(inner)
            if translated.startswith("(") and translated.endswith(")"):
                return f"{translated}.clone()"
            return f"({translated}).clone()"
        # Deref of pointer+index: (*t)[i].
        m = re.fullmatch(r"\(\*(\w+)\)\[(.*)\]", text, re.S)
        if m:
            var = m.group(1)
            assert self.symbols.get(var, "").startswith("Option"), text
            return f"{var}.as_ref().unwrap()[{self.expr(m.group(2))}]"
        # Deref of in_/getpos.
        m = re.fullmatch(r"\*(\w+)->in_", text)
        if m:
            return f"{m.group(1)}.current_byte().unwrap()"
        m = re.fullmatch(r"\*(\w+)->getpos\(\)", text)
        if m:
            return f"{m.group(1)}.current_byte().unwrap()"
        # Postfix ++ on iterator deref: *it++.
        m = re.fullmatch(r"\*(\w+)\+\+", text)
        if m:
            return f"{m.group(1)}.next().unwrap()"
        # Enum paths.
        m = re.fullmatch(r"Value::(\w+)", text)
        if m:
            name = m.group(1)
            if name in VALUE_TYPES:
                return f"ValueType::{VALUE_TYPES[name]}"
            assert name in UNIT_ENUMS, text
            return f"Unit::{UNIT_ENUMS[name]}"
        m = re.fullmatch(r"Property::(\w+)", text)
        if m:
            return f"Prop::{camel(m.group(1))}"
        m = re.fullmatch(r"Identifier::(\w+)", text)
        if m:
            return f"Ident::{camel(m.group(1))}"
        m = re.fullmatch(r"Parser::ErrorNumber\((.*)\)", text, re.S)
        if m:
            return f"crate::ast::error_number({self.expr(m.group(1))})"
        m = re.fullmatch(r"Parser::ValueToRGB\((.*)\)", text, re.S)
        if m:
            arg = m.group(1).strip()
            if arg == "NULL":
                return "Parser::value_to_rgb(None)"
            assert arg.startswith("&"), text
            return f"Parser::value_to_rgb(Some(&{self.expr(arg[1:])}))"
        m = re.fullmatch(r"Parser::(\w+)", text)
        if m:
            assert m.group(1) in ERROR_FLAGS, text
            return ERROR_FLAGS[m.group(1)]
        m = re.fullmatch(r"kHtmlTag(\w+)", text)
        if m:
            return f"HtmlTag::{m.group(1)}"
        m = re.fullmatch(r"SimpleSelector::(\w+)", text)
        if m:
            return f"SimpleSelectorType::{camel(m.group(1))}"
        m = re.fullmatch(r"SimpleSelectors::(\w+)", text)
        if m:
            return f"Combinator::{camel(m.group(1))}"
        m = re.fullmatch(r"FunctionParameters::(\w+)", text)
        if m:
            assert m.group(1) in ("COMMA_SEPARATED", "SPACE_SEPARATED"), text
            return "Separator::Comma" if m.group(1).startswith("COMMA") \
                else "Separator::Space"
        m = re.fullmatch(r"MediaQuery::(\w+)", text)
        if m:
            assert m.group(1) in ("NOT", "ONLY", "NO_QUALIFIER"), text
            return {"NOT": "MediaQualifier::Not",
                    "ONLY": "MediaQualifier::Only",
                    "NO_QUALIFIER": "MediaQualifier::None"}[m.group(1)]
        # Binary operators at depth 0 (conditions).
        split = split_binop(text)
        if split:
            left, op, right = split
            return f"{self.expr(left)} {op} {self.expr(right)}"
        # Function calls / method calls / member access.
        call = split_call(text)
        if call:
            receiver, arrow, method, argstr = call
            if receiver is None:
                args = split_top(argstr, ",")
                return self.free_call(
                    method, [a for a in args if a.strip() != ""])
            return self.method_call(receiver, method, argstr, arrow)
        m = re.fullmatch(r"(\w+)->(\w+)", text)
        if m:
            return self.member(m.group(1), m.group(2), True)
        m = re.fullmatch(r"(\w+)\.(\w+)", text)
        if m:
            return self.member(m.group(1), m.group(2), False)
        # Deref of an Option for by-ref params: *ptr.
        m = re.fullmatch(r"\*(\w+)", text)
        if m:
            return f"{m.group(1)}.as_mut().unwrap()"
        # Member with index: params[0].
        m = re.fullmatch(r"(\w+)\[(.*)\]", text, re.S)
        if m:
            return f"{m.group(1)}[{self.expr(m.group(2))}]"
        # Bare variable.
        if re.fullmatch(r"\w+", text):
            return text
        # Parenthesized.
        if text.startswith("(") and text.endswith(")"):
            return f"({self.expr(text[1:-1])})"
        raise AssertionError(f"unhandled expr: {text!r}")

    def split_binop(self, text):
        """Split one binary operation at depth 0 (longest op first)."""
        depth, instr, inchar, esc = 0, False, False, False
        i = 0
        ops = ["!=", "==", "<=", ">=", "&&", "||"]
        while i < len(text):
            c = text[i]
            if instr:
                if esc:
                    esc = False
                elif c == "\\":
                    esc = True
                elif c == '"':
                    instr = False
            elif inchar:
                if esc:
                    esc = False
                elif c == "\\":
                    esc = True
                elif c == "'":
                    inchar = False
            elif c == '"':
                instr = True
            elif c == "'":
                inchar = True
            elif c in "([{":
                depth += 1
            elif c in ")]}":
                depth -= 1
            elif depth == 0:
                for op in ops:
                    if text.startswith(op, i):
                        return text[:i], op, text[i + len(op):]
                if c in "<>&+-*/%":
                    # Avoid splitting -> and :: and template <>
                    if c == "-" and text.startswith("->", i):
                        pass
                    elif c == ":" or (c == "<" and i > 0 and text[i-1] == ":"):
                        pass
                    else:
                        return text[:i], c, text[i + 1:]
            i += 1
        return None

    def recv(self, receiver, arrow):
        """Translate a method-call receiver to a Rust expression."""
        receiver = receiver.strip()
        if receiver.startswith("*"):
            # Outer deref (`*(p->f())`): a no-op on references.
            return self.recv(receiver[1:], arrow)
        m = re.fullmatch(r"\(\*(\w+)\)\[(.*)\]", receiver, re.S)
        if m:
            return f"{m.group(1)}.as_ref().unwrap()[{self.expr(m.group(2))}]"
        m = re.fullmatch(r"(.*)\[([^\[\]]*)\]", receiver, re.S)
        if m:
            inner = m.group(1).strip()
            if re.fullmatch(r"\w+", inner):
                return f"{inner}[{self.expr(m.group(2))}]"
            return f"{self.expr(inner)}[{self.expr(m.group(2))}]"
        if arrow:
            kind = self.symbols.get(receiver, "")
            if kind == "Parser" or kind.startswith("&"):
                return receiver
            if kind.startswith("Option"):
                return f"{receiver}.as_ref().unwrap()"
            m = re.fullmatch(r"(.*)(->|\.)(\w+)\((.*)\)", receiver, re.S)
            if m:
                return self.method_call(
                    m.group(1), m.group(3), m.group(4), m.group(2) == "->")
            m = re.fullmatch(r"(.*)(->|\.)(\w+)", receiver)
            if m:
                return self.member(m.group(1), m.group(3), m.group(2) == "->")
            raise AssertionError(f"unknown receiver: {receiver!r}")
        if re.fullmatch(r"\w+", receiver):
            return receiver
        return self.expr(receiver)

    def method_call(self, receiver, method, argstr, arrow):
        assert method in METHODS, f"{receiver}->{method}"
        mapped = METHODS[method]
        args = [a for a in split_top(argstr, ",") if a.strip() != ""]
        if method in ("get", "at") and args:
            assert len(args) == 1, (receiver, method)
            return f"{self.recv(receiver, arrow)}[{self.expr(args[0])}]"
        if mapped == "GET":
            assert not args, (receiver, method)
            var = receiver.strip()
            assert self.symbols.get(var, "").startswith("Option"), receiver
            return f"{var}.as_ref()"
        if mapped == "RESET":
            raise AssertionError("reset in expression position")
        if mapped == "RBEGIN":
            return f"{self.recv(receiver, arrow)}.iter().rev()"
        if mapped == "IN_":
            return f"{receiver.strip()}.remaining()"
        if mapped == "GETPOS":
            raise AssertionError("bare getpos")
        if mapped == "to_css_string":
            var = receiver.strip()
            kind = self.symbols.get(var, "")
            free = {"Option<Values>": "values_to_css_string",
                    "Values": "values_to_css_string",
                    "Option<Declarations>": "declarations_to_css_string",
                    "Option<MediaQueries>": "media_queries_to_css_string",
                    "MediaQueries": "media_queries_to_css_string",
                    "Option<Selector>": "selector_to_css_string",
                    "Selector": "selector_to_css_string"}.get(kind)
            if free:
                recv = self.recv(receiver, arrow)
                if not recv.endswith("unwrap()"):
                    recv = f"&{recv}"
                return f"{free}({recv})"
        rust_args = []
        if mapped == "equals":
            return f"{self.recv(receiver, arrow)}.{mapped}" + \
                f"({', '.join(f'&({self.expr(a)})' for a in args)})"
        for index, arg in enumerate(args):
            arg = arg.strip()
            if mapped == "parse_font_family":
                m = re.fullmatch(r"(\w+)\.get\(\)", arg)
                if m:
                    rust_args.append(f"{m.group(1)}.as_mut().unwrap()")
                elif arg.startswith("&"):
                    rust_args.append(f"&mut {self.expr(arg[1:])}")
                elif re.fullmatch(r"\w+", arg):
                    rust_args.append(f"&mut {arg}")
                else:
                    rust_args.append(self.expr(arg))
            elif mapped == "parse_statement" and index == 1:
                # &mut Stylesheet parameter.
                m = re.fullmatch(r"(\w+)\.get\(\)", arg)
                if m:
                    rust_args.append(f"{m.group(1)}.as_mut().unwrap()")
                elif arg.startswith("*"):
                    rust_args.append(
                        f"{self.expr(arg[1:])}.as_mut().unwrap()")
                else:
                    rust_args.append(self.expr(arg))
            elif mapped == "parse_statement" and index == 0 \
                    and arg.startswith("&"):
                # Option<&MediaQueries> parameter.
                rust_args.append(f"Some(&{self.expr(arg[1:])})")
            elif arg.startswith("&"):
                rust_args.append(f"&mut {self.expr(arg[1:])}")
            else:
                rust_args.append(self.expr(arg))
        return f"{self.recv(receiver, arrow)}.{mapped}({', '.join(rust_args)})"

    def member(self, receiver, member, arrow):
        assert member in METHODS, f"{receiver}.{member}"
        mapped = METHODS[member]
        if mapped == "IN_":
            return f"{receiver.strip()}.remaining()"
        return f"{self.recv(receiver, arrow)}.{mapped}()"

    def free_call(self, name, args):
        if name in ("TestUnescape", "TestIdent", "TestDstring", "TestSstring",
                    "TestAnyNum", "TestAnyNumOtherUnit", "TestAnyString",
                    "TestAnyIdent", "TestAnyOtherIdent",
                    "TestBackgroundPosition", "TrapEOF", "SkipPast",
                    "FailureSkipPast"):
            snake = {"TestUnescape": "test_unescape", "TestIdent": "test_ident",
                     "TestDstring": "test_dstring", "TestSstring": "test_sstring",
                     "TestAnyNum": "test_any_num",
                     "TestAnyNumOtherUnit": "test_any_num_other_unit",
                     "TestAnyString": "test_any_string",
                     "TestAnyIdent": "test_any_ident",
                     "TestAnyOtherIdent": "test_any_other_ident",
                     "TestBackgroundPosition": "test_background_position",
                     "TrapEOF": "trap_eof", "SkipPast": "skip_past",
                     "FailureSkipPast": "failure_skip_past"}[name]
            if name == "TrapEOF" and len(args) == 1:
                return f"{snake}({self.expr(args[0])}, MethodToTest::ParseStylesheet)"
            if name == "TestUnescape" and len(args) == 3 \
                    and args[2].strip().startswith("'"):
                fixed = [self.expr(args[0]), self.expr(args[1]),
                         f"u32::from({self.expr(args[2])})"]
                return f"{snake}({', '.join(fixed)})"
            if name in ("TestAnyNum", "TestAnyNumOtherUnit"):
                num = args[2].strip()
                if re.fullmatch(r"-?\d+", num):
                    args = [args[0], args[1], num + ".0"] + args[3:]
            if name in ("SkipPast", "FailureSkipPast", "TrapEOF",
                          "TestUnescape", "TestIdent", "TestDstring",
                          "TestSstring", "TestAnyNum", "TestAnyNumOtherUnit",
                          "TestAnyString", "TestAnyIdent", "TestAnyOtherIdent",
                          "TestBackgroundPosition"):
                fixed = []
                for arg in args:
                    arg = arg.strip()
                    if arg in self.symbols and self.symbols[arg] == "vec":
                        fixed.append(f"&{arg}")
                    else:
                        fixed.append(self.expr(arg))
                if name == "TrapEOF" and len(fixed) == 1:
                    return f"{snake}({fixed[0]}, MethodToTest::ParseStylesheet)"
                if name == "TrapEOF":
                    method = {"PARSE_STYLESHEET": "ParseStylesheet",
                              "PARSE_CHARSET": "ParseCharset",
                              "EXTRACT_CHARSET": "ExtractCharset"}[
                                  args[1].strip()]
                    return f"{snake}({fixed[0]}, MethodToTest::{method})"
                return f"{snake}({', '.join(fixed)})"
            if name == "ParseAndExpandBackground":
                raise AssertionError("handled separately")
            return f"{snake}({', '.join(self.expr(a) for a in args)})"
        if name == "ParseAndExpandBackground":
            assert len(args) in (1, 2), args
            quirks = self.expr(args[1]) if len(args) == 2 else "true"
            return f"parse_and_expand_background({self.expr(args[0])}, {quirks})"
        if name == "Parser":
            # Direct construction in expression position (rare).
            return f"Parser::new({', '.join(self.expr(a) for a in args)})"
        if name == "ParseAny":
            # Fixture accessor for the private method; direct here.
            assert len(args) == 1 and args[0].strip().startswith("&"), args
            return f"{args[0].strip()[1:]}.parse_any()"
        if name == "Value":
            # Value(num, Unit) or Value(ident).
            assert len(args) == 2 or len(args) == 1, args
            if len(args) == 2:
                return f"Value::number({self.expr(args[0])}, {self.expr(args[1])})"
            return f"Value::ident({self.expr(args[0])})"
        if name == "Identifier":
            assert len(args) == 1, args
            return f"Identifier::from_ident({self.expr(args[0])})"
        if name == "ErrorNumber":
            return f"crate::ast::error_number({self.expr(args[0])})"
        raise AssertionError(f"unknown call: {name}")

    def wrap_option(self, arg):
        """Wrap infallible-constructor expressions in Some()."""
        text, rust = arg.strip(), self.expr(arg)
        m = re.fullmatch(r"(?:\w+->|\w+\.)?(\w+)\(.*\)", text, re.S)
        callee = METHODS.get(m.group(1), m.group(1)) if m else ""
        if callee in ALREADY_OPTION or text == "NULL" or \
                callee in ("parse_and_expand_background",):
            return rust
        if callee in VALUE_CALLEES:
            raise AssertionError(f"value in Option slot: {text!r}")
        if re.fullmatch(r"\w+", text):
            # Bare variable: already an Option or bool; leave alone.
            return rust
        return f"Some({rust})"


RUST_TYPES = {
    "Parser": "Parser", "Value": "Value", "Values": "Values",
    "Declarations": "Declarations", "Declaration": "Declaration",
    "Stylesheet": "Stylesheet", "Import": "Import",
    "MediaQueries": "MediaQueries", "MediaQuery": "MediaQuery",
    "Ruleset": "Ruleset", "Selectors": "Selectors",
    "Selector": "Selector", "SimpleSelectors": "SimpleSelectors",
    "SimpleSelector": "SimpleSelector", "UnicodeText": "UnicodeText",
    "FunctionParameters": "FunctionParameters",
}


def snake(name):
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


class Statements(Translator):
    def __init__(self):
        super().__init__()
        self.lines = []
        self.indent = 1

    def emit(self, text):
        self.lines.append("    " * self.indent + text)

    def block(self, statements):
        for stmt in statements:
            self.statement(stmt)

    def split_body(self, body):
        """Split a body into statements and {...} blocks."""
        items = []
        depth, cur, instr, inchar, esc = 0, [], False, False, False
        header = None
        i = 0
        buf = []
        while i < len(body):
            c = body[i]
            if instr:
                buf.append(c)
                if esc:
                    esc = False
                elif c == "\\":
                    esc = True
                elif c == '"':
                    instr = False
            elif inchar:
                buf.append(c)
                if esc:
                    esc = False
                elif c == "\\":
                    esc = True
                elif c == "'":
                    inchar = False
            elif c == '"':
                instr = True
                buf.append(c)
            elif c == "'":
                inchar = True
                buf.append(c)
            elif c == "{":
                if depth == 0:
                    header = "".join(buf).strip()
                    buf = []
                else:
                    buf.append(c)
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    items.append((header, "".join(buf)))
                    buf = []
                    header = None
                else:
                    buf.append(c)
            elif c in "([":
                depth += 1
                buf.append(c)
            elif c in ")]":
                depth -= 1
                buf.append(c)
            elif c == ";" and depth == 0:
                text = "".join(buf).strip()
                if text:
                    items.append((None, text))
                buf = []
            else:
                buf.append(c)
            i += 1
        rest = "".join(buf).strip()
        assert not rest, rest
        return items

    def statement(self, item):
        header, body = item
        if header is not None:
            self.block_statement(header, body)
            return
        stmt = body.strip()
        if not stmt:
            return
        # SCOPED_TRACE -> drop.
        if re.fullmatch(r"SCOPED_TRACE\(.*\)", stmt, re.S):
            return
        # EXPECT_* (trailing << "message" is dropped).
        m = re.match(r"(EXPECT|ASSERT)_(\w+)\(", stmt)
        if m:
            argstr, rest = self.expect_args(stmt[m.end() - 1:])
            assert rest.strip() == "" or rest.strip().startswith("<<"), stmt
            self.expectation(m.group(2), argstr)
            return
        # Static const arrays.
        m = re.fullmatch(
            r"(?:static\s+)?const char\* (\w+)\[\]\s*=\s*\{(.*)\}",
            stmt, re.S)
        if m:
            name, elements = m.group(1), split_top(m.group(2), ",")
            rust_elements = []
            for element in elements:
                element = element.strip()
                if element:
                    rust_elements.append(f"{self.expr(element)} as &[u8]")
            self.emit(f"let {name} = [{', '.join(rust_elements)}];")
            self.symbols[name] = "array"
            return
        # static const char NAME[] = "...".
        m = re.fullmatch(r"(?:static\s+)?const char (\w+)\[\]\s*=\s*(.*)",
                         stmt, re.S)
        if m:
            self.emit(f"let {m.group(1)} = {self.expr(m.group(2))};")
            self.symbols[m.group(1)] = "bytes"
            return
        # scoped_ptr<T> name; / scoped_ptr<T> name(expr);
        m = re.fullmatch(r"scoped_ptr<(\w+)> (\w+)(?:\((.*)\))?;", stmt, re.S)
        if not m:
            m = re.fullmatch(r"scoped_ptr<(\w+)> (\w+)(?:\((.*)\))?", stmt, re.S)
        if m:
            ctype, name, init = m.group(1), m.group(2), m.group(3)
            assert ctype in RUST_TYPES, stmt
            rust = RUST_TYPES[ctype]
            if ctype == "Parser":
                if init is None:
                    self.emit(f"let mut {name}: Parser;")
                else:
                    self.emit(f"let mut {name} = {self.expr(init.strip())};")
                self.symbols[name] = "Parser"
            else:
                if init is None:
                    self.emit(f"let mut {name}: Option<{rust}>;")
                else:
                    self.emit(
                        f"let mut {name} = {self.wrap_option(init.strip())};")
                self.symbols[name] = f"Option<{rust}>"
            return
        # Parser name(expr);
        m = re.fullmatch(r"Parser (\w+)\((.*)\)", stmt, re.S)
        if m:
            self.emit(f"let mut {m.group(1)} = Parser::new({self.expr(m.group(2))});")
            self.symbols[m.group(1)] = "Parser"
            return
        # Type name; (Values / UnicodeText / string locals).
        m = re.fullmatch(r"(Values|UnicodeText) (\w+);", stmt)
        if not m:
            m = re.fullmatch(r"(Values|UnicodeText) (\w+)", stmt)
        if m:
            ctype, name = m.group(1), m.group(2)
            if ctype == "Values":
                self.emit(f"let mut {name}: Values = Vec::new();")
            else:
                self.emit(f"let mut {name} = UnicodeText::new();")
            self.symbols[name] = ctype
            return
        # Type name(args) direct construction.
        m = re.fullmatch(r"string (\w+)\((\d+), (.*)\)", stmt, re.S)
        if m:
            name, count, char = m.group(1), m.group(2), m.group(3)
            self.emit(f"let {name} = vec![{self.expr(char)}; {count}];")
            self.symbols[name] = "vec"
            return
        m = re.fullmatch(r"(Value|Identifier|UnicodeText|string) (\w+)\((.*)\)",
                         stmt, re.S)
        if m:
            ctype, name, argstr = m.group(1), m.group(2), m.group(3)
            args = [a for a in split_top(argstr, ",") if a.strip() != ""]
            if ctype == "Value" and len(args) == 2:
                self.emit(f"let {name} = Value::number({self.expr(args[0])}, "
                          f"{self.expr(args[1])});")
            elif ctype == "Value" and len(args) == 1:
                self.emit(f"let {name} = Value::ident({self.expr(args[0])});")
            elif ctype == "Identifier" and len(args) == 1:
                self.emit(f"let {name} = Identifier::from_ident("
                          f"{self.expr(args[0])});")
            elif ctype == "UnicodeText" and len(args) == 1:
                self.emit(f"let {name} = UnicodeText::from_utf8("
                          f"{self.expr(args[0])});")
            else:
                raise AssertionError(stmt)
            self.symbols[name] = ctype
            return
        # SimpleSelector::const_reverse_iterator ...
        m = re.fullmatch(
            r"SimpleSelectors::const_reverse_iterator (\w+) = (.*)", stmt, re.S)
        if m:
            init = m.group(2).strip()
            rm = re.fullmatch(r"(\w+)->rbegin\(\)", init)
            assert rm, stmt
            var = rm.group(1)
            self.emit(f"let mut {m.group(1)} = {var}.as_ref().unwrap().iter().rev();")
            self.symbols[m.group(1)] = "reviter"
            return
        # Type name = expr; (int/bool/double/char32/const char*/auto).
        m = re.fullmatch(
            r"((?:const\s+)?(?:int|bool|double|float|char32|char|unsigned|string|auto"
            r"|Value::Unit|Identifier::Ident|Property::Prop))\s+(\w+)\s*=\s*(.*)",
            stmt, re.S)
        if m:
            ctype, name, init = m.group(1), m.group(2), m.group(3)
            self.emit(f"let {name} = {self.expr(init)};")
            self.symbols[name] = ctype
            return
        # reset / method-statement / assignment.
        m = re.fullmatch(r"(\w+)\.reset\((.*)\)", stmt, re.S)
        if m:
            name, init = m.group(1), m.group(2).strip()
            if self.symbols.get(name) == "Parser":
                self.emit(f"{name} = {self.expr(init)};")
            else:
                self.emit(f"{name} = {self.wrap_option(init)};")
            return
        m = re.fullmatch(r"delete (.*)", stmt, re.S)
        if m:
            self.emit(f"let _ = {self.expr(m.group(1))};")
            return
        m = re.fullmatch(
            r"(?:const\s+)?[\w:]+\s*([*&])\s*(\w+)\s*=\s*(.*)", stmt, re.S)
        if m:
            star, name, init = m.group(1), m.group(2), m.group(3).strip()
            if star == "&":
                # References are never null: bind directly.
                self.emit(f"let {name} = {self.expr(init)};")
                self.symbols[name] = "&"
                return
            step = re.fullmatch(r"\*(\w+)\+\+", init)
            if step:
                # Iterator step: already an Option.
                self.emit(f"let {name} = {step.group(1)}.next();")
            else:
                self.emit(f"let {name} = Some(&({self.expr(init)}));")
            self.symbols[name] = "Option<&>"
            return
        m = re.fullmatch(r"(\w+)\s*=\s*(.*)", stmt, re.S)
        if m and "(" not in m.group(1):
            self.emit(f"{m.group(1)} = {self.expr(m.group(2))};")
            return
        # Expression statement (method call or helper call).
        if re.fullmatch(r"\w+(?:->|\.).*", stmt, re.S):
            translated = self.expr(stmt)
            if translated.endswith(".to_css_string()"):
                self.emit(f"let _ = {translated};")
            else:
                self.emit(f"{translated};")
            return
        if re.fullmatch(r"\w+\(.*\)", stmt, re.S):
            self.emit(f"{self.expr(stmt)};")
            return
        raise AssertionError(f"unhandled statement: {stmt!r}")

    def expect_args(self, text):
        """Splits `(ARGS)REST` at the matching close paren."""
        assert text.startswith("(")
        depth, instr, inchar, esc = 0, False, False, False
        for i, c in enumerate(text):
            if instr:
                if esc:
                    esc = False
                elif c == "\\":
                    esc = True
                elif c == '"':
                    instr = False
            elif inchar:
                if esc:
                    esc = False
                elif c == "\\":
                    esc = True
                elif c == "'":
                    inchar = False
            elif c == '"':
                instr = True
            elif c == "'":
                inchar = True
            elif c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
                if depth == 0:
                    return text[1:i], text[i + 1:]
        raise AssertionError(text)

    def block_statement(self, header, body):
        header = header.strip()
        m = re.fullmatch(
            r"(?:static\s+)?const char\* (\w+)\[\] =", header)
        if m:
            elements = split_top(body, ",")
            rust_elements = []
            for element in elements:
                element = element.strip()
                if element:
                    rust_elements.append(f"{self.expr(element)} as &[u8]")
            self.emit(f"let {m.group(1)} = [{', '.join(rust_elements)}];")
            self.symbols[m.group(1)] = "array"
            return
        if header == "":
            self.emit("{")
            self.indent += 1
            self.block(self.split_body(body))
            self.indent -= 1
            self.emit("}")
            return
        m = re.fullmatch(
            r"for \(int (\w+) = 0; \1 < (.*); \+\+\1\)", header)
        if m:
            var, bound = m.group(1), m.group(2).strip()
            m2 = re.fullmatch(r"arraysize\((\w+)\)", bound)
            if m2:
                arr = m2.group(1)
                start = len(self.lines)
                self.emit(f"for case in {arr} {{")
                self.indent += 1
                self.block(self.split_body(body))
                self.indent -= 1
                self.emit("}")
                for i in range(start, len(self.lines)):
                    self.lines[i] = re.sub(
                        rf"\b{arr}\[{var}\]", "case", self.lines[i])
                return
            bound = self.expr(bound)
            self.emit(f"for {var} in 0..{bound} {{")
            self.indent += 1
            self.block(self.split_body(body))
            self.indent -= 1
            self.emit("}")
            return
        m = re.fullmatch(r"if \((.*)\)", header, re.S)
        if m:
            self.emit(f"if {self.condition(m.group(1))} {{")
            self.indent += 1
            self.block(self.split_body(body))
            self.indent -= 1
            self.emit("}")
            return
        raise AssertionError(f"unhandled block: {header!r}")

    def condition(self, text):
        text = text.strip()
        # X.get() != NULL / == NULL, X != NULL / == NULL.
        m = re.fullmatch(r"(.*)\.get\(\) != NULL", text, re.S)
        if m:
            return f"{m.group(1).strip()}.is_some()"
        m = re.fullmatch(r"(.*)\.get\(\) == NULL", text, re.S)
        if m:
            return f"{m.group(1).strip()}.is_none()"
        m = re.fullmatch(r"(\w+) != NULL", text)
        if m:
            return f"{m.group(1)}.is_some()"
        m = re.fullmatch(r"(\w+) == NULL", text)
        if m:
            return f"{m.group(1)}.is_none()"
        return self.expr(text)

    def is_bytes_side(self, side):
        side = side.strip()
        if side.startswith('b"'):
            return True
        return re.fullmatch(r"\w+", side) and \
            self.symbols.get(side) == "bytes"

    def is_vec_call(self, side):
        return any(key in side for key in
                   ("to_css_string(", "prop_text(",
                    "dimension_unit_text("))

    def expectation(self, kind, argstr):
        args = [a for a in split_top(argstr, ",") if a.strip() != ""]
        if kind in ("EQ", "NE"):
            assert len(args) == 2, argstr
            left_raw, right_raw = args[0].strip(), args[1].strip()
            if kind == "EQ" and left_raw in ("true", "false"):
                sense = "" if left_raw == "true" else "!"
                self.emit(f"assert!({sense}({self.expr(right_raw)}));")
                return
            if kind == "EQ" and right_raw in ("true", "false"):
                sense = "" if right_raw == "true" else "!"
                self.emit(f"assert!({sense}({self.expr(left_raw)}));")
                return
            left, right = self.expr(args[0]), self.expr(args[1])
            # usize rule for length/offset comparisons.
            if ".len()" in left or ".len()" in right or \
                    "current_offset()" in left or "current_offset()" in right:
                if re.fullmatch(r"\d+", left.strip()):
                    left = left.strip() + "usize"
                if re.fullmatch(r"\d+", right.strip()):
                    right = right.strip() + "usize"
            # UnicodeText owned-vs-borrowed: clone the call side.
            if left.startswith("UnicodeText::from_utf8"):
                right = f"({right}).clone()"
            elif right.startswith("UnicodeText::from_utf8"):
                left = f"({left}).clone()"
            # Vec-producing side compares as a slice against bytes.
            op = "assert_eq!" if kind == "EQ" else "assert_ne!"
            if self.is_bytes_side(left) and self.is_vec_call(right):
                self.emit(f"{op}({right}.as_slice(), {left} as &[u8]);")
                return
            if self.is_bytes_side(right) and self.is_vec_call(left):
                self.emit(f"{op}({left}.as_slice(), {right} as &[u8]);")
                return
            self.emit(f"{op}({left}, {right});")
        elif kind in ("TRUE", "FALSE"):
            assert len(args) == 1, argstr
            cond = args[0].strip()
            m = re.fullmatch(r"(\w+) == \w+->rend\(\)", cond)
            if m:
                sense = "" if kind == "TRUE" else "!"
                self.emit(f"assert!({sense}{m.group(1)}.next().is_none());")
                return
            m = re.fullmatch(r"(.*) != NULL", cond, re.S)
            if m:
                inner = self.expr(m.group(1))
                sense = "" if kind == "TRUE" else "!"
                self.emit(f"assert!({sense}({inner}).is_some());")
                return
            m = re.fullmatch(r"(.*) == NULL", cond, re.S)
            if m:
                inner = self.expr(m.group(1))
                sense = "" if kind == "TRUE" else "!"
                self.emit(f"assert!({sense}({inner}).is_none());")
                return
            m = re.fullmatch(r"NULL != (.*)", cond, re.S)
            if m:
                inner = self.expr(m.group(1))
                sense = "" if kind == "TRUE" else "!"
                self.emit(f"assert!({sense}({inner}).is_some());")
                return
            m = re.fullmatch(r"NULL == (.*)", cond, re.S)
            if m:
                inner = self.expr(m.group(1))
                sense = "" if kind == "TRUE" else "!"
                self.emit(f"assert!({sense}({inner}).is_none());")
                return
            m = re.fullmatch(r"([\w.>-]+)\.get\(\)", cond)
            if m:
                inner = m.group(1).strip()
                self.emit(f"assert!({inner}.is_{'some' if kind == 'TRUE' else 'none'}());")
                return
            if cond.strip().startswith("ParseAndExpandBackground("):
                inner = self.expr(cond)
                sense = "some" if kind == "TRUE" else "none"
                self.emit(f"assert!({inner}.is_{sense}());")
                return
            translated = self.expr(cond)
            if " & " in translated and "&&" not in translated:
                translated = f"({translated}) != 0"
            if kind == "FALSE":
                translated = f"!({translated})"
            self.emit(f"assert!({translated});")
        elif kind == "STREQ":
            assert len(args) == 2, argstr
            left, right = self.expr(args[0]), self.expr(args[1])
            if self.is_bytes_side(left) and self.is_vec_call(right):
                self.emit(f"assert_eq!({right}.as_slice(), {left} as &[u8]);")
                return
            if self.is_bytes_side(right) and self.is_vec_call(left):
                self.emit(f"assert_eq!({left}.as_slice(), {right} as &[u8]);")
                return
            self.emit(f"assert_eq!({left}, {right});")
        elif kind in ("DOUBLE_EQ", "FLOAT_EQ"):
            assert len(args) == 2, argstr
            left, right = self.expr(args[0]), self.expr(args[1])
            if re.fullmatch(r"-?\d+", left.strip()):
                left = left.strip() + ".0"
            if re.fullmatch(r"-?\d+", right.strip()):
                right = right.strip() + ".0"
            self.emit(f"assert_ulp_eq({left}, {right});")
        else:
            raise AssertionError(f"unhandled expectation: {kind}")


PREAMBLE = '''//! Ported C++ parser suite (white-box).
//!
//! Mechanically translated from `parser_unittest.cc` (see
//! `tool/translate_parser_tests.py`); do not hand-edit expectations.
//! The fixture helpers below are hand-ported once; test bodies are
//! generated.

#![allow(unused_mut, unused_assignments, unused_variables,
          non_snake_case, clippy::unnecessary_unwrap,
          clippy::approx_constant)]

use super::*;
#[allow(unused_imports)]
use crate::ast::{
    AT_RULE_ERROR, BLOCK_ERROR, CHARSET_ERROR, CSS_COMMENT_ERROR,
    DECLARATION_ERROR, FUNCTION_ERROR, HTML_COMMENT_ERROR, IMPORT_ERROR,
    MEDIA_ERROR, NO_ERROR, NUMBER_ERROR, RULESET_ERROR, SELECTOR_ERROR,
    SKIPPED_TOKEN_ERROR, UTF8_ERROR, VALUE_ERROR,
};
use crate::identifier::{Ident, Identifier};
use crate::tags::HtmlTag;
use crate::media::MediaQualifier;
use crate::property::{Prop, Property};
use crate::ast::declarations_to_css_string;
use crate::media::media_queries_to_css_string;
use crate::unicode_text::UnicodeText;
use crate::value::{
    Unit, Value, ValueType, Values, values_to_css_string,
};

/// gtest `EXPECT_DOUBLE_EQ` semantics: equal within 4 ULPs.
fn assert_ulp_eq(left: f64, right: f64) {
    if left == right {
        return;
    }
    let left_bits = left.to_bits() as i64;
    let right_bits = right.to_bits() as i64;
    if (left_bits < 0) != (right_bits < 0) {
        panic!("not almost equal: {left} vs {right}");
    }
    assert!(
        (left_bits - right_bits).abs() <= 4,
        "not almost equal: {left} vs {right}"
    );
}

fn test_unescape(input: &[u8], parselen: i32, value: u32) {
    let mut parser = Parser::new(input);
    let expected_len =
        if parselen == -1 { input.len() } else { parselen as usize };
    assert_eq!(value, parser.parse_escape());
    assert_eq!(expected_len, parser.current_offset());
}

fn test_ident(input: &[u8], parselen: i32, golden: &[u8]) {
    let mut parser = Parser::new(input);
    let expected_len =
        if parselen == -1 { input.len() } else { parselen as usize };
    assert_eq!(golden, parser.parse_ident().as_bytes());
    assert_eq!(expected_len, parser.current_offset());
}

fn test_dstring(input: &[u8], parselen: i32, golden: &[u8]) {
    let mut parser = Parser::new(input);
    let expected_len =
        if parselen == -1 { input.len() } else { parselen as usize };
    assert_eq!(golden, parser.parse_string(b'"').as_bytes());
    assert_eq!(expected_len, parser.current_offset());
}

fn test_sstring(input: &[u8], parselen: i32, golden: &[u8]) {
    let mut parser = Parser::new(input);
    let expected_len =
        if parselen == -1 { input.len() } else { parselen as usize };
    assert_eq!(golden, parser.parse_string(b'\\'').as_bytes());
    assert_eq!(expected_len, parser.current_offset());
}

#[allow(clippy::too_many_arguments)]
fn test_any_num(
    input: &[u8],
    parselen: i32,
    num: f64,
    unit: Unit,
    preservation: bool,
    verbatim: &[u8],
) {
    let mut parser = Parser::new(input);
    parser.set_preservation_mode(preservation);
    let expected_len =
        if parselen == -1 { input.len() } else { parselen as usize };
    let value = parser.parse_any().expect("value");
    assert_eq!(value.lexical_type(), ValueType::Number);
    assert_eq!(value.dimension(), unit);
    assert_ulp_eq(value.float_value(), num);
    assert_eq!(expected_len, parser.current_offset());
    assert_eq!(verbatim, value.bytes_in_original_buffer());
}

fn test_any_num_other_unit(
    input: &[u8],
    parselen: i32,
    num: f64,
    unit: &[u8],
) {
    // NOTE: upstream accepts the golden number but never checks it.
    let _ = num;
    let mut parser = Parser::new(input);
    let expected_len =
        if parselen == -1 { input.len() } else { parselen as usize };
    let value = parser.parse_any().expect("value");
    assert_eq!(value.lexical_type(), ValueType::Number);
    assert_eq!(value.dimension(), Unit::Other);
    assert_eq!(unit, value.dimension_unit_text().as_slice());
    assert_eq!(expected_len, parser.current_offset());
}

fn test_any_string(
    input: &[u8],
    parselen: i32,
    kind: ValueType,
    golden: &[u8],
) {
    let mut parser = Parser::new(input);
    let expected_len =
        if parselen == -1 { input.len() } else { parselen as usize };
    let value = parser.parse_any().expect("value");
    assert_eq!(kind, value.lexical_type());
    assert_eq!(golden, value.string_value().as_bytes());
    assert_eq!(expected_len, parser.current_offset());
}

fn test_any_ident(input: &[u8], parselen: i32, ident: Ident) {
    let mut parser = Parser::new(input);
    let expected_len =
        if parselen == -1 { input.len() } else { parselen as usize };
    let value = parser.parse_any().expect("value");
    assert_eq!(ValueType::Ident, value.lexical_type());
    assert_eq!(ident, value.identifier().ident());
    assert_eq!(expected_len, parser.current_offset());
}

fn test_any_other_ident(input: &[u8], parselen: i32, golden: &[u8]) {
    let mut parser = Parser::new(input);
    let expected_len =
        if parselen == -1 { input.len() } else { parselen as usize };
    let value = parser.parse_any().expect("value");
    assert_eq!(ValueType::Ident, value.lexical_type());
    assert_eq!(Ident::Other, value.identifier().ident());
    assert_eq!(golden, value.identifier_text().as_bytes());
    assert_eq!(expected_len, parser.current_offset());
}

fn parse_and_expand_background(
    input: &[u8],
    quirks: bool,
) -> Option<Declarations> {
    let mut parser = Parser::new(input);
    parser.set_quirks_mode(quirks);
    let values = parser.parse_values(Prop::Background)?;
    if values.is_empty() {
        return None;
    }
    let background = Declaration::new(
        Property::from_prop(Prop::Background),
        values,
        false,
    );
    let mut declarations = Declarations::new();
    expand_background(&background, &mut declarations);
    if declarations.is_empty() {
        return None;
    }
    Some(declarations)
}

fn test_background_position(input: &[u8], x: &[u8], y: &[u8]) {
    let declarations =
        parse_and_expand_background(input, true).expect("decls");
    let mut found_x = false;
    let mut found_y = false;
    for declaration in &declarations {
        match declaration.prop() {
            Prop::BackgroundPositionX => {
                assert_eq!(x, declaration.values()[0].to_css_string());
                found_x = true;
            }
            Prop::BackgroundPositionY => {
                assert_eq!(y, declaration.values()[0].to_css_string());
                found_y = true;
            }
            _ => {}
        }
    }
    assert!(found_x);
    assert!(found_y);
}

enum MethodToTest {
    ParseStylesheet,
    ParseCharset,
    ExtractCharset,
}

fn trap_eof(contents: &[u8], method: MethodToTest) {
    // Copy into an owned buffer (never NUL-terminated) so an
    // over-read cannot hide behind a terminator.
    let owned = contents.to_vec();
    let mut parser = Parser::new(&owned);
    match method {
        MethodToTest::ParseStylesheet => {
            let _ = parser.parse_stylesheet();
            assert_ne!(NO_ERROR, parser.errors_seen_mask());
        }
        MethodToTest::ParseCharset => {
            let _ = parser.parse_charset();
        }
        MethodToTest::ExtractCharset => {
            let _ = parser.extract_charset();
        }
    }
}

fn skip_past(delim: u8, input: &[u8]) -> &[u8] {
    let mut parser = Parser::new(input);
    assert!(parser.skip_past_delimiter(delim));
    // Re-slice from the input: the cursor survives the parser.
    &input[parser.current_offset()..]
}

fn failure_skip_past(delim: u8, input: &[u8]) {
    let mut parser = Parser::new(input);
    assert!(!parser.skip_past_delimiter(delim));
    assert!(parser.done());
}

#[test]
fn bounds_copied_media_queries() {
    // Hand-written: builds the input programmatically.
    let mut css = b"@media screen and (min-width: ".to_vec();
    css.extend(std::iter::repeat_n(b'1', 1024));
    css.extend_from_slice(b"px) {");
    for _ in 0..5000 {
        css.extend_from_slice(b".a{}");
    }
    css.extend_from_slice(b"}");
    let mut parser = Parser::new(&css);
    let _ = parser.parse_stylesheet();
    assert_ne!(NO_ERROR, parser.errors_seen_mask());
    assert!((MEDIA_ERROR & parser.errors_seen_mask()) != 0);
}

// DIVERGENCE (unescape): the committed test expects `\\\\abcdef` to
// decode to a space, but `UniLib::IsInterchangeValid(0xABCDEF)`
// returns true (verified against compiled `unilib.cc`), so the
// committed `ParseEscape` returns 0xABCDEF. The expectation below is
// actual C++ behavior; downstream `push_back` still coerces the
// out-of-range value to a space, so parsed output is unaffected.
#[test]
fn unescape() {
    test_unescape(b"\\\\abcdef aabc", 8, 0xABCDEF);
    test_unescape(b"\\\\A", 2, 0xA);
    test_unescape(b"\\\\A0b5C\\r\\n", 8, 0xa0b5c);
    test_unescape(b"\\\\AB ", 4, 0xAB);
}

// DIVERGENCE (value_validation): the committed test expects the
// parser to drop non-color `border-color` values, but the committed
// parser has no such validation (`ParseAnyExpectingColor` falls back
// to `ParseAny`), keeping all 10 declarations with no errors —
// verified by compiling and running the vendored parser. The passing
// minify goldens (e.g. `width: 10` kept verbatim) confirm the
// no-validation behavior, so the port follows the code, not the
// stale expectation.
#[test]
fn value_validation() {
    let mut a = Parser::new(b"width: {$width}");
    let _ = a.parse_declarations();
    a = Parser::new(b"border-color: \\\"string\\\"; border-color: url(\\\"abc\\\"); border-color: 12345; border-color: none; border-color: inherited; border-color: red; border-color: #123456; border-color: transparent; border-color: inherit; border-color: unknown; ");
    let t = Some(a.parse_declarations());
    assert_eq!(NO_ERROR, a.errors_seen_mask());
    assert_eq!(10usize, t.as_ref().unwrap().len());
    let expected = [
        b"border-color: \\\"string\\\"".as_slice(),
        b"border-color: url(abc)".as_slice(),
        b"border-color: 12345".as_slice(),
        b"border-color: none".as_slice(),
        b"border-color: inherited".as_slice(),
        b"border-color: #ff0000".as_slice(),
        b"border-color: #123456".as_slice(),
        b"border-color: transparent".as_slice(),
        b"border-color: inherit".as_slice(),
        b"border-color: unknown".as_slice(),
    ];
    for (index, golden) in expected.iter().enumerate() {
        assert_eq!(
            t.as_ref().unwrap()[index].to_css_string().as_slice(),
            *golden
        );
    }
}

// DIVERGENCE (accept_all_values): same root cause as
// value_validation — non-preservation mode keeps (not drops) the
// declaration, with no error. Verified against the compiled parser.
#[test]
fn accept_all_values() {
    let mut p = Parser::new(b"display: -moz-inline-box");
    p.set_preservation_mode(true);
    let mut declarations = Some(p.parse_declarations());
    assert_eq!(NO_ERROR, p.errors_seen_mask());
    assert_eq!(1usize, declarations.as_ref().unwrap().len());
    assert_eq!(1usize, declarations.as_ref().unwrap()[0].values().len());
    let value = Some(&(declarations.as_ref().unwrap()[0].values()[0]));
    assert_eq!(ValueType::Ident, value.as_ref().unwrap().lexical_type());
    assert_eq!(Ident::Other, value.as_ref().unwrap().identifier().ident());
    assert_eq!(b"-moz-inline-box", (value.as_ref().unwrap().identifier().ident_text()).as_bytes());
    assert_eq!(declarations_to_css_string(declarations.as_ref().unwrap()).as_slice(), b"display: -moz-inline-box" as &[u8]);
    let mut p2 = Parser::new(b"display: -moz-inline-box");
    p2.set_preservation_mode(false);
    declarations = Some(p2.parse_declarations());
    assert_eq!(NO_ERROR, p2.errors_seen_mask());
    assert_eq!(1usize, declarations.as_ref().unwrap().len());
    assert_eq!(declarations_to_css_string(declarations.as_ref().unwrap()).as_slice(), b"display: -moz-inline-box" as &[u8]);
}

// DIVERGENCE (at_font_face): the committed expectation drops the
// `font-variant` declaration, but the committed parser keeps it
// (verified byte-identical against the compiled parser, mask 130 in
// both). Only that segment differs; the rest is unchanged.
#[test]
fn at_font_face() {
    let mut parser: Parser;
    let mut stylesheet: Option<Stylesheet>;
    parser = Parser::new(b"@font-face{font-family:'Ubuntu';font-style:normal}\\n.foo { width: 1px; }");
    stylesheet = Some(parser.parse_stylesheet());
    assert_eq!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(stylesheet.as_ref().unwrap().to_css_string().as_slice(), b"/* AUTHOR */\\n\\n\\n@font-face { font-family: \\\"Ubuntu\\\"; font-style: normal }\\n.foo {width: 1px}\\n" as &[u8]);
    parser = Parser::new(b"@font-face{font-family:'Ubuntu';font-style:normal}.foo { width: 1px; }");
    parser.set_preservation_mode(true);
    stylesheet = Some(parser.parse_stylesheet());
    assert_eq!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(stylesheet.as_ref().unwrap().to_css_string().as_slice(), b"/* AUTHOR */\\n\\n\\n@font-face { font-family: \\\"Ubuntu\\\"; font-style: normal }\\n.foo {width: 1px}\\n" as &[u8]);
    parser = Parser::new(b"@media print {\\n  .foo { width: 1px; }\\n  @font-face { font-family: 'Ubuntu'; font-style: normal; }\\n  .bar { height: 2em; }\\n}\\n");
    stylesheet = Some(parser.parse_stylesheet());
    assert_eq!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(stylesheet.as_ref().unwrap().to_css_string().as_slice(), b"/* AUTHOR */\\n\\n\\n@media print { @font-face { font-family: \\\"Ubuntu\\\"; font-style: normal } }\\n@media print { .foo {width: 1px} }\\n@media print { .bar {height: 2em} }\\n" as &[u8]);
    parser = Parser::new(b"@media print {\\n  @font-face { font-family: 'Dothraki'; src: local('Khal'), url('dothraki.woff') format('woff'); }\\n}\\n");
    stylesheet = Some(parser.parse_stylesheet());
    assert_eq!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(stylesheet.as_ref().unwrap().to_css_string().as_slice(), b"/* AUTHOR */\\n\\n\\n@media print { @font-face { font-family: \\\"Dothraki\\\"; src: local(\\\"Khal\\\") , url(dothraki.woff) format(\\\"woff\\\") } }\\n\\n" as &[u8]);
    parser = Parser::new(b"@font-face {\\n  font-family: MainText;\\n  src: url(gentium.eot);\\n  src: local(\\\"Gentium\\\"), url('gentium.ttf') format('truetype'), url(gentium.woff);\\n  font-style: italic;\\n  font-weight: 800;\\n  font-stretch: ultra-condensed;\\n  unicode-range: U+590-5ff, u+4??, U+1F63B;\\n  font-variant: historical-forms, character-variant(cv13), annotiation(circled);\\n  font-feature-settings: 'hwid', 'swsh' 2;\\n}\\n");
    stylesheet = Some(parser.parse_stylesheet());
    assert_ne!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(stylesheet.as_ref().unwrap().to_css_string().as_slice(), b"/* AUTHOR */\\n\\n\\n@font-face { font-family: MainText; src: url(gentium.eot); src: local(\\\"Gentium\\\") , url(gentium.ttf) format(\\\"truetype\\\") , url(gentium.woff); font-style: italic; font-weight: 800; font-stretch: ultra-condensed; font-variant: historical-forms , character-variant(cv13) , annotiation(circled); font-feature-settings: \\\"hwid\\\" , \\\"swsh\\\" 2 }\\n\\n" as &[u8]);
}

'''


def split_binop(text):
    """Split one depth-0 binary operator (multi-char first)."""
    depth, instr, inchar, esc = 0, False, False, False
    chars = list(text)
    i = 0
    while i < len(chars):
        c = chars[i]
        if instr:
            if esc:
                esc = False
            elif c == "\\":
                esc = True
            elif c == '"':
                instr = False
        elif inchar:
            if esc:
                esc = False
            elif c == "\\":
                esc = True
            elif c == "'":
                inchar = False
        elif c == '"':
            instr = True
        elif c == "'":
            closer = text.find("'", i + 1, i + 5)
            if closer != -1:
                inchar = True
        elif c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
        elif depth == 0:
            if not text[:i].strip():
                i += 1
                continue
            two = text[i:i + 2]
            if two in ("==", "!=", "&&", "||", "<=", ">="):
                return text[:i], two, text[i + 2:]
            if c == "&" and text[i + 1:i + 2] != "&" \
                    and text[i - 1:i] != "&":
                return text[:i], "&", text[i + 1:]
            if c == "<" and not text.startswith("<<", i):
                return text[:i], "<", text[i + 1:]
            if c == ">" and not text.startswith(">>", i) \
                    and text[i - 1:i] != "-":
                return text[:i], ">", text[i + 1:]
        i += 1
    return None


def split_call(text):
    """Split `RECEIVER.METHOD(ARGS)` at the outermost call.

    Returns (receiver or None, arrow, method, argstr), or None when
    the text is not a call.
    """
    text = text.strip()
    if not text.endswith(")"):
        return None
    # Find the '(' matching the final ')' (string/char aware).
    depth, instr, inchar, esc = 0, False, False, False
    last_open = -1
    for i, c in enumerate(text):
        if instr:
            if esc:
                esc = False
            elif c == "\\":
                esc = True
            elif c == '"':
                instr = False
        elif inchar:
            if esc:
                esc = False
            elif c == "\\":
                esc = True
            elif c == "'":
                inchar = False
        elif c == '"':
            instr = True
        elif c == "'":
            closer = text.find("'", i + 1, i + 5)
            if closer != -1:
                inchar = True
        elif c == "(":
            if depth == 0:
                last_open = i
            depth += 1
        elif c == ")":
            depth -= 1
    if last_open < 0:
        return None
    argstr, head = text[last_open + 1:-1], text[:last_open].strip()
    # Method chains may break across lines after -> or . (raw
    # newlines never occur inside C++ literals, so this is safe).
    head = re.sub(r"(->|\.)[ \t]*\n\s*", r"\1", head)
    m = re.fullmatch(r"(.*)(->|\.)(\w+)", head, re.S)
    if m:
        return (m.group(1), m.group(2) == "->", m.group(3), argstr)
    m = re.fullmatch(r"(\w+)", head)
    if m:
        return (None, False, m.group(1), argstr)
    return None


def normalize_breaks(body):
    # Method chains may break across lines after -> or . (raw
    # newlines never occur inside C++ literals, so this is safe).
    return re.sub(r"(->|\.)[ \t]*\n\s*", r"\1", body)


def main():
    src = open(sys.argv[1]).read()
    bodies = re.findall(r"TEST_F\(ParserTest, (\w+)\) \{(.*?)\n\}\n", src, re.S)
    assert bodies, "no tests found"
    out = [PREAMBLE]
    count = 0
    for name, body in bodies:
        if name in SKIP:
            continue
        translator = Statements()
        translator.lines.append("#[test]")
        translator.lines.append(f"fn {snake(name)}() {{")
        translator.block(
            translator.split_body(normalize_breaks(strip_comments(body))))
        translator.lines.append("}")
        out.append("\n".join(translator.lines))
        out.append("")
        count += 1
    with open(sys.argv[2], "w") as handle:
        handle.write("\n".join(out))
    print(f"translated {count} tests ({len(SKIP)} skipped as hand-written)")


main()
