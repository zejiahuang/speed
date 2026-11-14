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

# The one callback that goes the other way: Rust calls this one *back*.
#
# `watt-ffi` holds the protector as a global reference and calls
# `env.call_method(target, "protect", "(I)Z")`. R8 cannot see that reference, and
# an abstract interface method with no Kotlin caller is exactly what it shaves
# off — along with the implementing lambda's body. The interface survives with
# zero methods and the first VPN connection fails in `GetMethodID`, which is a
# crash rather than a caught error because the JNI call reports it as a missing
# method. The rule above is not enough: it names methods *exported from* the
# library, not the one the library calls.
-keep interface dev.detour.core.Kernel$Protector {
    *;
}

# The call is virtual, so the runtime class is the lambda, not the interface.
# Keeping the interface member named `protect` also pins the override's name
# (dispatch would break otherwise), and this rule keeps the member itself from
# being shrunk out of the implementor.
-keepclassmembers class * implements dev.detour.core.Kernel$Protector {
    boolean protect(int);
}
