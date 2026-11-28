package dev.vetro.app

/** What a pairing QR carries (ARCHITECTURE section 8): `vetro://pair?fp=..&t=..&a=..&p=..`. */
class PairUri(
    val fingerprint: ByteArray, // 32 bytes
    val token: ByteArray, // 16 bytes, secret: never log it
    val addresses: List<String>,
    val port: Int,
) {
    // The token must not leak through a log line that prints the object.
    override fun toString() = "PairUri(addresses=$addresses, port=$port)"

    companion object {
        private const val PREFIX = "vetro://pair?"
        private val FIELDS = setOf("fp", "t", "a", "p")
        private const val OCTET = "(25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)"
        private val IPV4 = Regex("$OCTET(\\.$OCTET){3}")

        // shortcut: IPv6 is only shape-checked; vetro-client rejects a bad one (nativeStart returns 0). Upgrade if the host ever listens on IPv6.
        private val IPV6 = Regex("[0-9a-fA-F:.]{2,45}")

        /** Null when [text] is not a valid pairing URI. Every field is required exactly once. */
        fun parse(text: String): PairUri? {
            val t = text.trim()
            if (!t.startsWith(PREFIX)) return null
            val fields = HashMap<String, String>()
            for (pair in t.removePrefix(PREFIX).split('&')) {
                val kv = pair.split('=')
                if (kv.size != 2 || kv[0] !in FIELDS) return null
                if (fields.put(kv[0], kv[1]) != null) return null
            }
            val fp = hex(fields["fp"], 32) ?: return null
            val token = hex(fields["t"], 16) ?: return null
            val addresses = (fields["a"] ?: return null).split(',')
            if (addresses.any { !isIp(it) }) return null
            val port = fields["p"]?.takeIf { it.length <= 5 && it.all { c -> c in '0'..'9' } }?.toInt()
            if (port == null || port !in 1..65535) return null
            return PairUri(fp, token, addresses, port)
        }

        private fun isIp(s: String) = IPV4.matches(s) || (s.count { it == ':' } >= 2 && IPV6.matches(s))

        /** [s] as exactly [len] bytes of hex (either case), else null. */
        fun hex(s: String?, len: Int): ByteArray? {
            if (s == null || s.length != len * 2) return null
            return ByteArray(len) { i ->
                val hi = nibble(s[2 * i])
                val lo = nibble(s[2 * i + 1])
                if (hi < 0 || lo < 0) return null
                (hi * 16 + lo).toByte()
            }
        }

        // ASCII only: Character.digit would also accept other scripts' digits.
        private fun nibble(c: Char) = when (c) {
            in '0'..'9' -> c - '0'
            in 'a'..'f' -> c - 'a' + 10
            in 'A'..'F' -> c - 'A' + 10
            else -> -1
        }

        fun toHex(b: ByteArray) = b.joinToString("") { "%02x".format(it) }
    }
}
