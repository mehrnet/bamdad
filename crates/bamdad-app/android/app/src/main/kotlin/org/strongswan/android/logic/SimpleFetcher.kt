package org.strongswan.android.logic

/**
 * Placeholder for the class the native bridge looks up during `JNI_OnLoad`.
 *
 * strongSwan's bridge resolves `org.strongswan.android.logic.SimpleFetcher` and
 * keeps a global reference, but Bamdad never calls into it (certificate
 * fetching is handled by the app if it is ever needed). Without the class,
 * `JNI_OnLoad` would fail and every library load would abort.
 */
class SimpleFetcher
