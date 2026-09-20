package com.remoteapp.client;

import android.app.NativeActivity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import java.security.KeyStore;
import java.util.Arrays;
import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

/** Native UI with a small platform-only bridge. Keys never leave Android Keystore. */
public final class RemoteActivity extends NativeActivity {
    private static final String ALIAS = "remoteapp.local.v1";
    private SecretKey key(boolean create) throws Exception {
        KeyStore store = KeyStore.getInstance("AndroidKeyStore");
        store.load(null);
        if (!store.containsAlias(ALIAS)) {
            if (!create) throw new IllegalStateException("Keystore key is missing");
            KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
            generator.init(new KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build());
            generator.generateKey();
        }
        return (SecretKey) store.getKey(ALIAS, null);
    }
    public byte[] protectKey(byte[] plaintext) throws Exception {
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.ENCRYPT_MODE, key(true));
        byte[] encrypted = cipher.doFinal(plaintext);
        byte[] nonce = cipher.getIV();
        byte[] output = new byte[1 + nonce.length + encrypted.length];
        output[0] = (byte) nonce.length;
        System.arraycopy(nonce, 0, output, 1, nonce.length);
        System.arraycopy(encrypted, 0, output, 1 + nonce.length, encrypted.length);
        return output;
    }
    public byte[] unprotectKey(byte[] payload) throws Exception {
        if (payload.length < 29 || payload[0] != 12) throw new IllegalArgumentException("Invalid key envelope");
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.DECRYPT_MODE, key(false), new GCMParameterSpec(128, Arrays.copyOfRange(payload, 1, 13)));
        return cipher.doFinal(payload, 13, payload.length - 13);
    }
    public String readClipboard() {
        ClipboardManager clipboard = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
        ClipData clip = clipboard.getPrimaryClip();
        if (clip == null || clip.getItemCount() == 0) return "";
        return clip.getItemAt(0).coerceToText(this).toString();
    }
    public void writeClipboard(String text) {
        ClipboardManager clipboard = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
        clipboard.setPrimaryClip(ClipData.newPlainText("RemoteAPP", text));
    }
}
