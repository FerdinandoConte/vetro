package dev.vetro.app

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat

/** Home (paired PCs) and the pairing screen (QR scan, or the pairing link typed or pasted). */
class MainActivity : ComponentActivity() {
    private val main = Handler(Looper.getMainLooper())
    private lateinit var store: HostStore
    private var hosts by mutableStateOf(emptyList<PairedHost>())
    private var pairing by mutableStateOf(false)
    private var link by mutableStateOf("")
    private var error by mutableStateOf<String?>(null)

    // The QR is read many times a second: start the stream once.
    private var starting = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        store = HostStore(this)
        takeLink(intent)
        setContent {
            MaterialTheme(colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme()) {
                Surface(Modifier.fillMaxSize()) {
                    if (pairing) PairScreen() else HomeScreen()
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        takeLink(intent)
    }

    override fun onResume() {
        super.onResume()
        starting = false
        hosts = store.all()
    }

    /** `vetro://pair?...` opened from outside (adb, a link) only fills the field: the owner confirms. */
    private fun takeLink(intent: Intent) {
        val data = intent.data?.toString() ?: return
        link = data
        error = null
        pairing = true
    }

    private fun startStream(host: String, port: Int, fingerprint: ByteArray, token: ByteArray?) {
        if (starting) return
        starting = true
        startActivity(
            Intent(this, StreamActivity::class.java)
                .putExtra(StreamActivity.EXTRA_HOST, host)
                .putExtra(StreamActivity.EXTRA_PORT, port)
                .putExtra(StreamActivity.EXTRA_FP, PairUri.toHex(fingerprint))
                .apply { token?.let { putExtra(StreamActivity.EXTRA_TOKEN, PairUri.toHex(it)) } },
        )
    }

    private fun pair(text: String) {
        val uri = PairUri.parse(text)
        if (uri == null) {
            error = "This is not a Vetro pairing code."
            return
        }
        // shortcut: only the first address is tried (the engine prints one); try them in turn when it lists several.
        startStream(uri.addresses.first(), uri.port, uri.fingerprint, uri.token)
    }

    @Composable
    private fun HomeScreen() {
        Column(Modifier.safeDrawingPadding().padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Vetro", style = MaterialTheme.typography.headlineMedium)
            if (hosts.isEmpty()) Text("No PC paired yet.")
            for (h in hosts) {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                    Column {
                        Text("${h.address}:${h.port}")
                        Text(PairUri.toHex(h.fingerprint).take(16), style = MaterialTheme.typography.bodySmall)
                    }
                    Row {
                        TextButton(onClick = { store.remove(h); hosts = store.all() }) { Text("Forget") }
                        Button(onClick = { startStream(h.address, h.port, h.fingerprint, null) }) { Text("Connect") }
                    }
                }
            }
            Button(onClick = { error = null; pairing = true }) { Text("Pair a new PC") }
        }
    }

    @Composable
    private fun PairScreen() {
        var granted by remember {
            mutableStateOf(
                ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED,
            )
        }
        val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted = it }
        LaunchedEffect(Unit) { if (!granted) ask.launch(Manifest.permission.CAMERA) }
        Column(Modifier.safeDrawingPadding().padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Pair a new PC", style = MaterialTheme.typography.headlineMedium)
            if (granted) {
                QrScanner(this@MainActivity, Modifier.fillMaxWidth().height(280.dp)) { text ->
                    main.post { pair(text) }
                }
            } else {
                Text("The camera is off. Type or paste the pairing link below instead.")
            }
            OutlinedTextField(
                value = link,
                onValueChange = { link = it; error = null },
                label = { Text("Pairing link (vetro://pair?...)") },
                modifier = Modifier.fillMaxWidth(),
            )
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Button(onClick = { pair(link) }) { Text("Pair") }
                TextButton(onClick = { pairing = false }) { Text("Back") }
            }
        }
    }
}
