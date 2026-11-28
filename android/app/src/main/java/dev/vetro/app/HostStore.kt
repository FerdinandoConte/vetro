package dev.vetro.app

import android.content.Context

/** A host the tablet is paired with. Public data only: the token is never stored. */
class PairedHost(val fingerprint: ByteArray, val address: String, val port: Int) {
    override fun equals(other: Any?) = other is PairedHost && fingerprint.contentEquals(other.fingerprint) &&
        address == other.address && port == other.port

    override fun hashCode() = (fingerprint.contentHashCode() * 31 + address.hashCode()) * 31 + port

    /** Stored value; the fingerprint (hex) is the key. */
    fun encode() = "$address|$port"

    companion object {
        fun decode(fpHex: String, value: String): PairedHost? {
            val fp = PairUri.hex(fpHex, 32) ?: return null
            val parts = value.split('|')
            val port = parts.getOrNull(1)?.toIntOrNull()
            if (parts.size != 2 || parts[0].isEmpty() || port == null || port !in 1..65535) return null
            return PairedHost(fp, parts[0], port)
        }
    }
}

/** Paired hosts in app-private SharedPreferences (not backed up: see the manifest). */
class HostStore(context: Context) {
    private val prefs = context.getSharedPreferences("hosts", Context.MODE_PRIVATE)

    fun all(): List<PairedHost> =
        prefs.all.mapNotNull { (k, v) -> (v as? String)?.let { PairedHost.decode(k, it) } }.sortedBy { it.address }

    /** Called only after `nativePaired()` returned true. A re-pairing replaces the address. */
    fun save(host: PairedHost) {
        prefs.edit().putString(PairUri.toHex(host.fingerprint), host.encode()).apply()
    }

    fun remove(host: PairedHost) {
        prefs.edit().remove(PairUri.toHex(host.fingerprint)).apply()
    }
}
