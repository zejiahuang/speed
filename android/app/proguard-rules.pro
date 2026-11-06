# JNI surface for the Rust kernel.
#
# `watt-ffi` exports `Java_dev_detour_core_Kernel_native*` symbols, and the Kotlin
# side declares them as `private external fun`. The lookup happens by name at
# runtime, so R8 must keep both the class name and the method names; renaming
# either produces an `UnsatisfiedLinkError` that only appears after install.
#
# AGP's default rules already keep `native` methods, but naming the surface here
# makes the dependency explicit and survives a change to those defaults.
-keepclasseswithmembernames,includedescriptorclasses class dev.detour.core.Kernel {
    native <methods>;
}
