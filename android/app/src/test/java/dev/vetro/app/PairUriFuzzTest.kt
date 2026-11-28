package dev.vetro.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.Random

/** Seeded, so a failure reproduces: the parser reads untrusted text from a QR or a typed field. */
class PairUriFuzzTest {
    private val rnd = Random(0x5eed)
    private val valid = "vetro://pair?fp=${"ab".repeat(32)}&t=${"01".repeat(16)}&a=192.168.1.20&p=47800"
    private val alphabet = "vetro://pair?fp=&t=a,p0123456789abcdefABCDEF:.-+ %\u0000é١"

    private fun check(s: String) {
        val u = PairUri.parse(s) ?: return // rejecting is always fine; throwing is not
        assertEquals(s, 32, u.fingerprint.size)
        assertEquals(s, 16, u.token.size)
        assertTrue(s, u.addresses.isNotEmpty() && u.addresses.none { it.isEmpty() })
        assertTrue(s, u.port in 1..65535)
    }

    @Test fun randomStringsNeverThrow() {
        repeat(20_000) {
            check(String(CharArray(rnd.nextInt(120)) { alphabet[rnd.nextInt(alphabet.length)] }))
        }
    }

    @Test fun mutationsOfAValidLinkNeverThrow() {
        check(valid)
        repeat(20_000) {
            val sb = StringBuilder(valid)
            repeat(1 + rnd.nextInt(2)) {
                val i = rnd.nextInt(sb.length)
                when (rnd.nextInt(3)) {
                    0 -> sb.setCharAt(i, alphabet[rnd.nextInt(alphabet.length)])
                    1 -> sb.deleteCharAt(i)
                    else -> sb.insert(i, alphabet[rnd.nextInt(alphabet.length)])
                }
            }
            check(sb.toString())
        }
    }

    @Test fun everyTruncationOfAValidLinkNeverThrows() {
        for (n in 0..valid.length) check(valid.substring(0, n))
    }
}
