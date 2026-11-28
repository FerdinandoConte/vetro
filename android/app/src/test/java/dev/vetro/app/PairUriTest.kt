package dev.vetro.app

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

class PairUriTest {
    private val fp = "ab".repeat(32)
    private val t = "0123456789abcdef0123456789ABCDEF"

    private fun uri(
        fp: String = this.fp,
        t: String = this.t,
        a: String = "192.168.1.20",
        p: String = "47800",
    ) = "vetro://pair?fp=$fp&t=$t&a=$a&p=$p"

    @Test fun parsesEngineOutput() {
        val u = PairUri.parse(uri())!!
        assertEquals(listOf("192.168.1.20"), u.addresses)
        assertEquals(47800, u.port)
        assertEquals(32, u.fingerprint.size)
        assertEquals(0xab.toByte(), u.fingerprint[0])
        assertArrayEquals(PairUri.hex(t, 16), u.token)
    }

    @Test fun acceptsSurroundingWhitespaceAndSeveralAddresses() {
        val u = PairUri.parse("  " + uri(a = "10.0.0.2,fe80::1") + "\n")!!
        assertEquals(listOf("10.0.0.2", "fe80::1"), u.addresses)
    }

    @Test fun rejectsBadFingerprint() {
        assertNull(PairUri.parse(uri(fp = "ab".repeat(31))))
        assertNull(PairUri.parse(uri(fp = "ab".repeat(33))))
        assertNull(PairUri.parse(uri(fp = "zz".repeat(32))))
        assertNull(PairUri.parse(uri(fp = "")))
    }

    @Test fun rejectsBadToken() {
        assertNull(PairUri.parse(uri(t = "ab".repeat(15))))
        assertNull(PairUri.parse(uri(t = "ab".repeat(17))))
        assertNull(PairUri.parse(uri(t = "g".repeat(32))))
    }

    @Test fun rejectsBadPort() {
        for (p in listOf("0", "65536", "-1", "abc", "", "+80", "123456", "99999999999")) {
            assertNull("port $p", PairUri.parse(uri(p = p)))
        }
        assertNotNull(PairUri.parse(uri(p = "65535")))
        assertNotNull(PairUri.parse(uri(p = "1")))
    }

    @Test fun rejectsBadAddress() {
        val bad = listOf("", "256.1.1.1", "1.2.3", "1.2.3.4.5", "pc.local", "01.2.3.4", "192.168.1.20,", "1.2.3.4,host")
        for (a in bad) assertNull("address $a", PairUri.parse(uri(a = a)))
    }

    @Test fun rejectsMissingDuplicateUnknownAndForeignFields() {
        assertNull(PairUri.parse("vetro://pair?fp=$fp&t=$t&a=1.2.3.4"))
        assertNull(PairUri.parse("vetro://pair?t=$t&a=1.2.3.4&p=1"))
        assertNull(PairUri.parse(uri() + "&p=47801"))
        assertNull(PairUri.parse(uri() + "&x=1"))
        assertNull(PairUri.parse("vetro://pair"))
        assertNull(PairUri.parse("vetro://pair?"))
        assertNull(PairUri.parse(uri().replace("vetro://", "https://")))
        assertNull(PairUri.parse(uri().replace("pair", "open")))
        assertNull(PairUri.parse(""))
    }

    @Test fun toStringNeverShowsTheToken() {
        assertFalse(PairUri.parse(uri())!!.toString().contains(t, ignoreCase = true))
    }
}
