/*
 * combase.c
 * 
 * This small shim is used to satisfy dependencies on Win7 x64 when using 
 * VC-LTL/YY-Thunks. It exports CoTaskMemFree which is sometimes routed 
 * through newer DLLs on Win10 but available in ole32 on Win7.
 */
#pragma comment(linker, "/export:CoTaskMemFree=ole32.CoTaskMemFree")
