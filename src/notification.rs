use std::error::Error;

/// Shows a notification with a title and body on the current platform.
pub fn notify(title: &str, body: &str) -> Result<(), Box<dyn Error>> {
    #[cfg(target_os = "android")]
    {
        android::notify(title, body)
    }

    #[cfg(not(target_os = "android"))]
    {
        notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .show()?;
        Ok(())
    }
}

#[cfg(target_os = "android")]
mod android {
    use jni::objects::{JObject, JValue};
    use jni::{JNIEnv, JavaVM, errors::Result as JniResult};
    use std::error::Error;

    const CHANNEL_ID: &str = "default";
    const CHANNEL_NAME: &str = "General";
    const IMPORTANCE_DEFAULT: i32 = 3;
    const NOTIFICATION_ID: i32 = 1;

    pub fn notify(title: &str, body: &str) -> Result<(), Box<dyn Error>> {
        let ctx = ndk_context::android_context();
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }?;
        let mut env = vm.attach_current_thread()?;
        let activity = unsafe { JObject::from_raw(ctx.context().cast()) };

        let result = post(&mut env, &activity, title, body);
        if result.is_err() {
            // A pending Java exception would make every later JNI call fail.
            let _ = env.exception_clear();
        }
        Ok(result?)
    }

    fn post(env: &mut JNIEnv, activity: &JObject, title: &str, body: &str) -> JniResult<()> {
        let service_name = env.new_string("notification")?;
        let manager = env
            .call_method(
                activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[(&service_name).into()],
            )?
            .l()?;

        // Channels are required on API 26+. Creating an existing one is a no-op.
        let channel_id = env.new_string(CHANNEL_ID)?;
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

        // Reuse the app's own launcher icon as the small icon.
        let app_info = env
            .call_method(
                activity,
                "getApplicationInfo",
                "()Landroid/content/pm/ApplicationInfo;",
                &[],
            )?
            .l()?;
        let icon = env.get_field(&app_info, "icon", "I")?.i()?;

        let builder = env.new_object(
            "android/app/Notification$Builder",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[activity.into(), (&channel_id).into()],
        )?;

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

        let notification = env
            .call_method(&builder, "build", "()Landroid/app/Notification;", &[])?
            .l()?;

        env.call_method(
            &manager,
            "notify",
            "(ILandroid/app/Notification;)V",
            &[JValue::Int(NOTIFICATION_ID), (&notification).into()],
        )?;

        Ok(())
    }
}
