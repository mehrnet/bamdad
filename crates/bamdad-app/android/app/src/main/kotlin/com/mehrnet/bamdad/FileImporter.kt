package com.mehrnet.bamdad

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.provider.OpenableColumns
import android.util.Log
import android.widget.EditText
import android.widget.Toast
import net.lingala.zip4j.ZipFile
import net.lingala.zip4j.exception.ZipException
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import kotlin.concurrent.thread

/**
 * File and ZIP import. Plain text configs are handed to Rust as-is; ZIPs are
 * expanded here so password-protected archives can prompt in the app.
 */
object FileImporter {
    private const val TAG = "BamdadImport"
    private const val MAX_FILE_BYTES = 2 * 1024 * 1024
    private const val MAX_ZIP_BYTES = 20 * 1024 * 1024
    private const val MAX_ENTRIES = 40

    fun pick(activity: Activity) {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "*/*"
            putExtra(
                Intent.EXTRA_MIME_TYPES,
                arrayOf(
                    "application/zip",
                    "application/x-zip-compressed",
                    "text/plain",
                    "application/json",
                    "application/octet-stream",
                ),
            )
        }
        activity.startActivityForResult(intent, MainActivity.IMPORT_FILE_REQUEST)
    }

    fun handle(activity: MainActivity, uri: Uri) {
        thread(name = "bamdad-import") {
            try {
                val bytes = activity.contentResolver.openInputStream(uri)?.use { it.readBytes() }
                    ?: return@thread
                if (bytes.size > MAX_ZIP_BYTES) {
                    toast(activity, "File is too large")
                    return@thread
                }
                val name = displayName(activity, uri) ?: "imported"
                if (isZip(bytes)) {
                    val file = File(activity.cacheDir, "bamdad-import.zip")
                    file.writeBytes(bytes)
                    handleZip(activity, file, name.removeSuffix(".zip").removeSuffix(".ZIP"))
                } else {
                    importPlain(name, bytes)
                }
            } catch (error: Throwable) {
                Log.e(TAG, "import failed", error)
                toast(activity, "Import failed: ${error.message}")
            }
        }
    }

    private fun isZip(bytes: ByteArray): Boolean =
        bytes.size >= 4 &&
            bytes[0] == 'P'.code.toByte() &&
            bytes[1] == 'K'.code.toByte() &&
            (bytes[2] == 3.toByte() || bytes[2] == 5.toByte())

    private fun handleZip(activity: MainActivity, file: File, group: String) {
        val zip = try {
            ZipFile(file)
        } catch (error: Throwable) {
            Toast.makeText(activity, "Invalid ZIP", Toast.LENGTH_LONG).show()
            return
        }
        if (zip.isEncrypted) {
            promptPassword(activity, zip, group, "This ZIP is password-protected")
        } else {
            extract(activity, zip, group, null)
        }
    }

    private fun promptPassword(
        activity: MainActivity,
        zip: ZipFile,
        group: String,
        message: String,
    ) {
        Handler(Looper.getMainLooper()).post {
            val input = EditText(activity).apply {
                hint = "ZIP password"
                setSingleLine(true)
                inputType = android.text.InputType.TYPE_CLASS_TEXT or
                    android.text.InputType.TYPE_TEXT_VARIATION_PASSWORD
            }
            AlertDialog.Builder(activity)
                .setTitle("ZIP password")
                .setMessage(message)
                .setView(input)
                .setPositiveButton("Import") { _, _ ->
                    val password = input.text.toString()
                    thread(name = "bamdad-zip") {
                        extract(activity, zip, group, password)
                    }
                }
                .setNegativeButton("Cancel", null)
                .show()
        }
    }

    private fun extract(activity: MainActivity, zip: ZipFile, group: String, password: String?) {
        try {
            if (!password.isNullOrEmpty()) {
                zip.setPassword(password.toCharArray())
            }
            val files = JSONArray()
            var count = 0
            for (header in zip.fileHeaders) {
                if (header.isDirectory) continue
                val entry = header.fileName.replace('\\', '/')
                if (entry.startsWith("__MACOSX/") || entry.substringAfterLast('/').startsWith(".")) {
                    continue
                }
                if (!supported(entry)) continue
                if (header.size > MAX_FILE_BYTES.toLong()) {
                    Log.w(TAG, "skipping oversized entry $entry (${header.size} bytes)")
                    continue
                }
                if (count >= MAX_ENTRIES) break
                val text = zip.getInputStream(header).bufferedReader().use { it.readText() }
                files.put(
                    JSONObject()
                        .put("name", entry.substringAfterLast('/'))
                        .put("text", text),
                )
                count++
            }
            if (files.length() == 0) {
                toast(activity, "No supported configs found in ZIP")
                return
            }
            MainActivity.nativeImportConfigs(group, files.toString())
            toast(activity, "Imported $count config${if (count == 1) "" else "s"}")
        } catch (error: ZipException) {
            if (error.type == ZipException.Type.WRONG_PASSWORD) {
                promptPassword(activity, zip, group, "Wrong password. Try again.")
            } else {
                Log.e(TAG, "zip extraction failed", error)
                toast(activity, "ZIP error: ${error.message}")
            }
        } catch (error: Throwable) {
            Log.e(TAG, "zip extraction failed", error)
            toast(activity, "ZIP error: ${error.message}")
        }
    }

    private fun importPlain(name: String, bytes: ByteArray) {
        val text = bytes.toString(Charsets.UTF_8)
        val files = JSONArray().put(
            JSONObject().put("name", name).put("text", text),
        )
        MainActivity.nativeImportConfigs("", files.toString())
    }

    private fun supported(name: String): Boolean {
        val lower = name.lowercase()
        return lower.endsWith(".conf") ||
            lower.endsWith(".ovpn") ||
            lower.endsWith(".sswan") ||
            lower.endsWith(".json") ||
            lower.endsWith(".txt")
    }

    private fun displayName(activity: Activity, uri: Uri): String? {
        return try {
            activity.contentResolver.query(uri, null, null, null, null)?.use { cursor ->
                val index = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                if (index >= 0 && cursor.moveToFirst()) cursor.getString(index) else null
            } ?: uri.lastPathSegment
        } catch (_: Throwable) {
            uri.lastPathSegment
        }
    }

    private fun toast(activity: Activity, message: String) {
        Handler(Looper.getMainLooper()).post {
            Toast.makeText(activity, message, Toast.LENGTH_LONG).show()
        }
    }
}
