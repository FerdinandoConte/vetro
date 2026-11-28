package dev.vetro.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class PairedHostTest {
    private val fp = "cd".repeat(32)

    @Test fun roundTrips() {
        val h = PairedHost(PairUri.hex(fp, 32)!!, "192.168.1.20", 47800)
        assertEquals(h, PairedHost.decode(fp, h.encode()))
    }

    @Test fun rejectsCorruptStorage() {
        assertNull(PairedHost.decode("zz", "1.2.3.4|47800"))
        assertNull(PairedHost.decode(fp, "1.2.3.4"))
        assertNull(PairedHost.decode(fp, "|47800"))
        assertNull(PairedHost.decode(fp, "1.2.3.4|0"))
        assertNull(PairedHost.decode(fp, "1.2.3.4|70000"))
        assertNull(PairedHost.decode(fp, "1.2.3.4|x"))
        assertNull(PairedHost.decode(fp, "1.2.3.4|1|2"))
    }
}
