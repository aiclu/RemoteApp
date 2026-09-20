//! OS services. Production platforms must never fall back to an in-memory key.
use std::{
    io::Write,
    path::{Path, PathBuf},
};
pub trait PlatformServices {
    fn data_dir(&self) -> Result<PathBuf, String>;
    fn protect(&self, bytes: &[u8]) -> Result<Vec<u8>, String>;
    fn unprotect(&self, bytes: &[u8]) -> Result<Vec<u8>, String>;
    fn read_clipboard(&self) -> Result<String, String>;
    fn write_clipboard(&self, text: &str) -> Result<(), String>;
}
pub struct NativePlatform;
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("pending");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(file);
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(&temporary, path).map_err(|e| e.to_string())?;
        std::fs::File::open(path.parent().ok_or("无效存储目录")?)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(windows)]
fn dpapi(bytes: &[u8], protect: bool) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::{Foundation::LocalFree, Security::Cryptography::*};
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len().try_into().map_err(|_| "数据过大")?,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let result =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData.cast());
    }
    Ok(result)
}

#[cfg(target_os = "android")]
fn android_call<T>(
    call: impl FnOnce(&mut jni::JNIEnv<'_>, &jni::objects::JObject<'_>) -> Result<T, String>,
) -> Result<T, String> {
    let context = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let activity = unsafe { jni::objects::JObject::from_raw(context.context().cast()) };
    let result = call(&mut env, &activity);
    // The activity reference belongs to android-activity, not to us.
    let _ = activity.into_raw();
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
        return Err("Android 安全存储或剪贴板调用失败".into());
    }
    result
}
#[cfg(target_os = "android")]
fn android_crypto(bytes: &[u8], method: &str) -> Result<Vec<u8>, String> {
    android_call(|env, activity| {
        let input = env
            .byte_array_from_slice(bytes)
            .map_err(|e| e.to_string())?;
        let result = env
            .call_method(
                activity,
                method,
                "([B)[B",
                &[jni::objects::JValue::Object(&input)],
            )
            .and_then(|v| v.l())
            .map_err(|e| e.to_string())?;
        env.convert_byte_array(jni::objects::JByteArray::from(result))
            .map_err(|e| e.to_string())
    })
}

impl PlatformServices for NativePlatform {
    fn data_dir(&self) -> Result<PathBuf, String> {
        #[cfg(windows)]
        {
            return std::env::var_os("LOCALAPPDATA")
                .map(|p| PathBuf::from(p).join("RemoteAPP"))
                .ok_or("LOCALAPPDATA 不可用".into());
        }
        #[cfg(target_os = "android")]
        {
            return android_call(|env, activity| {
                let file = env
                    .call_method(activity, "getFilesDir", "()Ljava/io/File;", &[])
                    .and_then(|v| v.l())
                    .map_err(|e| e.to_string())?;
                let path = env
                    .call_method(file, "getAbsolutePath", "()Ljava/lang/String;", &[])
                    .and_then(|v| v.l())
                    .map_err(|e| e.to_string())?;
                let path = jni::objects::JString::from(path);
                let value: String = env.get_string(&path).map_err(|e| e.to_string())?.into();
                Ok(PathBuf::from(value))
            });
        }
        #[cfg(not(any(windows, target_os = "android")))]
        {
            Err("此预览平台尚未提供安全存储".into())
        }
    }
    fn protect(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        #[cfg(windows)]
        {
            return dpapi(bytes, true);
        }
        #[cfg(target_os = "android")]
        {
            return android_crypto(bytes, "protectKey");
        }
        #[cfg(not(any(windows, target_os = "android")))]
        {
            let _ = bytes;
            Err("安全存储不可用".into())
        }
    }
    fn unprotect(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        #[cfg(windows)]
        {
            return dpapi(bytes, false);
        }
        #[cfg(target_os = "android")]
        {
            return android_crypto(bytes, "unprotectKey");
        }
        #[cfg(not(any(windows, target_os = "android")))]
        {
            let _ = bytes;
            Err("安全存储不可用".into())
        }
    }
    fn read_clipboard(&self) -> Result<String, String> {
        #[cfg(target_os = "android")]
        {
            return android_call(|env, activity| {
                let value = env
                    .call_method(activity, "readClipboard", "()Ljava/lang/String;", &[])
                    .and_then(|v| v.l())
                    .map_err(|e| e.to_string())?;
                let value = jni::objects::JString::from(value);
                let text = env.get_string(&value).map_err(|e| e.to_string())?.into();
                Ok(text)
            });
        }
        #[cfg(not(target_os = "android"))]
        {
            Err("桌面预览请在剪贴板面板中粘贴文本".into())
        }
    }
    fn write_clipboard(&self, text: &str) -> Result<(), String> {
        #[cfg(target_os = "android")]
        {
            return android_call(|env, activity| {
                let text = env.new_string(text).map_err(|e| e.to_string())?;
                env.call_method(
                    activity,
                    "writeClipboard",
                    "(Ljava/lang/String;)V",
                    &[jni::objects::JValue::Object(&text)],
                )
                .map_err(|e| e.to_string())?;
                Ok(())
            });
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = text;
            Err("桌面预览请从剪贴板面板复制文本".into())
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn dpapi_round_trip_and_tamper_rejection() {
        let secret = b"temporary-vault-key-material";
        let mut protected = NativePlatform.protect(secret).unwrap();
        assert_ne!(protected, secret);
        assert_eq!(NativePlatform.unprotect(&protected).unwrap(), secret);
        let last = protected.len() - 1;
        protected[last] ^= 1;
        assert!(NativePlatform.unprotect(&protected).is_err());
    }
}
