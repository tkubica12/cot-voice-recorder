# Keep Moshi/Room/WorkManager reflection-safe members.
-keep class com.tomaskubica.voiceprompt.data.api.** { *; }
-keepclassmembers class ** {
    @com.squareup.moshi.FromJson *;
    @com.squareup.moshi.ToJson *;
}
-keep class kotlin.Metadata { *; }

# Moshi
-dontwarn org.jetbrains.annotations.**
-keep class com.squareup.moshi.** { *; }
-keepnames class kotlinx.coroutines.internal.MainDispatcherFactory {}

# WorkManager instantiates workers reflectively
-keep class * extends androidx.work.ListenableWorker {
    public <init>(android.content.Context, androidx.work.WorkerParameters);
}
