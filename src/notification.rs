use std::error::Error;

/// Stable notification id derived from the rule identity, so separate rules
/// can be shown at the same time while repeated warnings for one rule replace
/// the previous notification instead of piling up. The identity is used
/// instead of the title because two rules may legitimately share a title.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn notification_id(key: &str) -> i32 {
    // FNV-1a keeps the id stable across processes and Rust versions.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in key.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    (hash & 0x7fff_ffff) as i32
}

/// Shows a notification with a title and body on the current platform.
pub fn notify(key: &str, title: &str, body: &str) -> Result<(), Box<dyn Error>> {
    #[cfg(target_os = "android")]
    {
        android::notify(key, title, body)
    }

    #[cfg(not(target_os = "android"))]
    {
        let _ = key;
        notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .show()?;
        Ok(())
    }
}

/// Shows a notification from a background Android component.
///
/// Background jobs cannot request runtime permissions, so this path only posts
/// when the permission was already granted by the foreground app.
#[cfg(target_os = "android")]
pub fn notify_with_context<'local>(
    env: &mut jni::JNIEnv<'local>,
    context: &jni::objects::JObject<'local>,
    key: &str,
    title: &str,
    body: &str,
) -> Result<(), Box<dyn Error>> {
    android::notify_with_context(env, context, key, title, body)
}

#[cfg(target_os = "android")]
mod android {
    use jni::objects::{JObject, JValue};
    use jni::{JNIEnv, JavaVM, errors::Result as JniResult};
    use std::error::Error;
    use std::sync::atomic::{AtomicBool, Ordering};

    static PERMISSION_REQUESTED: AtomicBool = AtomicBool::new(false);

    const CHANNEL_ID: &str = "default";
    const CHANNEL_NAME: &str = "General";
    const IMPORTANCE_DEFAULT: i32 = 3;
    const PRIORITY_DEFAULT: i32 = 0;
    const POST_NOTIFICATIONS: &str = "android.permission.POST_NOTIFICATIONS";
    const PERMISSION_GRANTED: i32 = 0;
    const PERMISSION_REQUEST_CODE: i32 = 1;

    pub fn notify(key: &str, title: &str, body: &str) -> Result<(), Box<dyn Error>> {
        let ctx = ndk_context::android_context();
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }?;
        let mut env = vm.attach_current_thread()?;
        let activity = unsafe { JObject::from_raw(ctx.context().cast()) };

        let result = post(&mut env, &activity, key, title, body, true);
        if result.is_err() {
            // A pending Java exception would make every later JNI call fail.
            let _ = env.exception_clear();
        }
        Ok(result?)
    }

    pub fn notify_with_context<'local>(
        env: &mut JNIEnv<'local>,
        context: &JObject<'local>,
        key: &str,
        title: &str,
        body: &str,
    ) -> Result<(), Box<dyn Error>> {
        let result = post(env, context, key, title, body, false);
        if result.is_err() {
            let _ = env.exception_clear();
        }
        Ok(result?)
    }

    fn post<'local>(
        env: &mut JNIEnv<'local>,
        context: &JObject<'local>,
        key: &str,
        title: &str,
        body: &str,
        request_permission: bool,
    ) -> JniResult<()> {
        let id = super::notification_id(key);
        let sdk = sdk_int(env)?;
        if request_permission {
            request_notification_permission(env, context, sdk);
        }

        let service_name = env.new_string("notification")?;
        let manager = env
            .call_method(
                context,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[(&service_name).into()],
            )?
            .l()?;

        // Channels are required on API 26+. Creating an existing one is a no-op.
        let channel_id = env.new_string(CHANNEL_ID)?;
        if sdk >= 26 {
            let channel_name = env.new_string(CHANNEL_NAME)?;
            let channel = env.new_object(
                "android/app/NotificationChannel",
                "(Ljava/lang/String;Ljava/lang/CharSequence;I)V",
                &[
                    (&channel_id).into(),
                    (&channel_name).into(),
                    JValue::Int(IMPORTANCE_DEFAULT),
                ],
            )?;
            env.call_method(
                &manager,
                "createNotificationChannel",
                "(Landroid/app/NotificationChannel;)V",
                &[(&channel).into()],
            )?;
        }

        // Reuse the app's own launcher icon as the small icon.
        let app_info = env
            .call_method(
                context,
                "getApplicationInfo",
                "()Landroid/content/pm/ApplicationInfo;",
                &[],
            )?
            .l()?;
        let icon = env.get_field(&app_info, "icon", "I")?.i()?;

        let builder = if sdk >= 26 {
            env.new_object(
                "android/app/Notification$Builder",
                "(Landroid/content/Context;Ljava/lang/String;)V",
                &[context.into(), (&channel_id).into()],
            )?
        } else {
            env.new_object(
                "android/app/Notification$Builder",
                "(Landroid/content/Context;)V",
                &[context.into()],
            )?
        };
        if sdk < 26 {
            // Pre-channel notifications use a priority instead of importance.
            env.call_method(
                &builder,
                "setPriority",
                "(I)Landroid/app/Notification$Builder;",
                &[JValue::Int(PRIORITY_DEFAULT)],
            )?;
        }

        let title = env.new_string(title)?;
        env.call_method(
            &builder,
            "setContentTitle",
            "(Ljava/lang/CharSequence;)Landroid/app/Notification$Builder;",
            &[(&title).into()],
        )?;

        let body = env.new_string(body)?;
        env.call_method(
            &builder,
            "setContentText",
            "(Ljava/lang/CharSequence;)Landroid/app/Notification$Builder;",
            &[(&body).into()],
        )?;

        env.call_method(
            &builder,
            "setSmallIcon",
            "(I)Landroid/app/Notification$Builder;",
            &[JValue::Int(icon)],
        )?;

        // Best effort: a missing launch intent must not block the warning.
        match content_intent(env, context, sdk) {
            Ok(Some(intent)) => {
                env.call_method(
                    &builder,
                    "setContentIntent",
                    "(Landroid/app/PendingIntent;)Landroid/app/Notification$Builder;",
                    &[(&intent).into()],
                )?;
            }
            Ok(None) => {}
            Err(_) => {
                let _ = env.exception_clear();
            }
        }

        let notification = env
            .call_method(&builder, "build", "()Landroid/app/Notification;", &[])?
            .l()?;

        env.call_method(
            &manager,
            "notify",
            "(ILandroid/app/Notification;)V",
            &[JValue::Int(id), (&notification).into()],
        )?;

        Ok(())
    }

    /// Builds a pending intent that opens the app when the notification is
    /// tapped. Returns `None` when the package has no launch intent.
    fn content_intent<'local>(
        env: &mut JNIEnv<'local>,
        context: &JObject<'local>,
        sdk: i32,
    ) -> JniResult<Option<JObject<'local>>> {
        let package = env
            .call_method(context, "getPackageName", "()Ljava/lang/String;", &[])?
            .l()?;
        let manager = env
            .call_method(
                context,
                "getPackageManager",
                "()Landroid/content/pm/PackageManager;",
                &[],
            )?
            .l()?;
        let launch = env
            .call_method(
                &manager,
                "getLaunchIntentForPackage",
                "(Ljava/lang/String;)Landroid/content/Intent;",
                &[(&package).into()],
            )?
            .l()?;
        if launch.is_null() {
            return Ok(None);
        }

        let mut flags: i32 = 0x0800_0000; // PendingIntent.FLAG_UPDATE_CURRENT
        if sdk >= 23 {
            flags |= 0x0400_0000; // PendingIntent.FLAG_IMMUTABLE
        }
        let intent = env
            .call_static_method(
                "android/app/PendingIntent",
                "getActivity",
                "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
                &[
                    context.into(),
                    JValue::Int(0),
                    (&launch).into(),
                    JValue::Int(flags),
                ],
            )?
            .l()?;
        Ok(Some(intent))
    }

    fn sdk_int(env: &mut JNIEnv) -> JniResult<i32> {
        env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?
            .i()
    }

    /// Android 13+ requires the runtime notification permission. Request it
    /// once; if the context is not an Activity the call fails harmlessly.
    fn request_notification_permission(env: &mut JNIEnv, activity: &JObject, sdk: i32) {
        if sdk < 33 {
            return;
        }

        let permission = match env.new_string(POST_NOTIFICATIONS) {
            Ok(permission) => permission,
            Err(_) => {
                let _ = env.exception_clear();
                return;
            }
        };
        let granted = env
            .call_method(
                activity,
                "checkSelfPermission",
                "(Ljava/lang/String;)I",
                &[(&permission).into()],
            )
            .and_then(|value| value.i());
        let Ok(granted) = granted else {
            let _ = env.exception_clear();
            return;
        };
        if granted == PERMISSION_GRANTED {
            return;
        }
        if PERMISSION_REQUESTED.swap(true, Ordering::Relaxed) {
            return;
        }

        let permissions = match env.new_object_array(1, "java/lang/String", &permission) {
            Ok(permissions) => permissions,
            Err(_) => {
                let _ = env.exception_clear();
                return;
            }
        };
        if env
            .call_method(
                activity,
                "requestPermissions",
                "([Ljava/lang/String;I)V",
                &[(&permissions).into(), JValue::Int(PERMISSION_REQUEST_CODE)],
            )
            .is_err()
        {
            let _ = env.exception_clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::notification_id;

    #[test]
    fn different_rules_get_different_notification_ids() {
        assert_ne!(notification_id("rule-work"), notification_id("rule-sleep"));
    }

    #[test]
    fn notification_ids_are_stable() {
        assert_eq!(notification_id("rule-work"), notification_id("rule-work"));
        assert!(notification_id("") >= 0);
    }

    #[test]
    fn rules_with_the_same_title_but_different_ids_do_not_collide() {
        // The key is the rule id, not the display title, so two rules named
        // "Overwork" still get separate notifications.
        assert_ne!(notification_id("rule-a"), notification_id("rule-b"));
    }
}
