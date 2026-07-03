// Demand-event spans are UTF-8 BYTE offsets into the evaluated module;
// CodeMirror positions are UTF-16 code units. An em-dash in a comment is
// enough to skew the two (3 bytes vs 1 unit), so convert properly. Sources
// are small; linear scans are fine.

function utf8Len(cp: number): number {
  if (cp <= 0x7f) return 1;
  if (cp <= 0x7ff) return 2;
  if (cp <= 0xffff) return 3;
  return 4;
}

/// UTF-8 byte offset → UTF-16 code-unit offset (clamped to the string).
export function byteToUtf16(s: string, byte: number): number {
  let bytes = 0;
  let i = 0;
  while (i < s.length && bytes < byte) {
    const cp = s.codePointAt(i)!;
    bytes += utf8Len(cp);
    i += cp > 0xffff ? 2 : 1;
  }
  return i;
}

/// UTF-16 code-unit offset → UTF-8 byte offset (clamped to the string).
export function utf16ToByte(s: string, utf16: number): number {
  let bytes = 0;
  let i = 0;
  while (i < s.length && i < utf16) {
    const cp = s.codePointAt(i)!;
    bytes += utf8Len(cp);
    i += cp > 0xffff ? 2 : 1;
  }
  return bytes;
}
