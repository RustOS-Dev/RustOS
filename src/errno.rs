//! Linux-compatible error numbers.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Errno(pub i32);

pub type KResult<T> = Result<T, Errno>;

macro_rules! errnos {
    ($($name:ident = $val:expr, $msg:expr;)*) => {
        $(pub const $name: Errno = Errno($val);)*
        impl Errno {
            pub fn message(self) -> &'static str {
                match self.0 {
                    $($val => $msg,)*
                    _ => "unknown error",
                }
            }
            pub fn name(self) -> &'static str {
                match self.0 {
                    $($val => stringify!($name),)*
                    _ => "E?",
                }
            }
        }
    };
}

errnos! {
    EPERM = 1, "Operation not permitted";
    ENOENT = 2, "No such file or directory";
    ESRCH = 3, "No such process";
    EINTR = 4, "Interrupted system call";
    EIO = 5, "I/O error";
    ENXIO = 6, "No such device or address";
    E2BIG = 7, "Argument list too long";
    ENOEXEC = 8, "Exec format error";
    EBADF = 9, "Bad file descriptor";
    ECHILD = 10, "No child processes";
    EAGAIN = 11, "Resource temporarily unavailable";
    ENOMEM = 12, "Cannot allocate memory";
    EACCES = 13, "Permission denied";
    EFAULT = 14, "Bad address";
    ENOTBLK = 15, "Block device required";
    EBUSY = 16, "Device or resource busy";
    EEXIST = 17, "File exists";
    EXDEV = 18, "Invalid cross-device link";
    ENODEV = 19, "No such device";
    ENOTDIR = 20, "Not a directory";
    EISDIR = 21, "Is a directory";
    EINVAL = 22, "Invalid argument";
    ENFILE = 23, "Too many open files in system";
    EMFILE = 24, "Too many open files";
    ENOTTY = 25, "Inappropriate ioctl for device";
    ETXTBSY = 26, "Text file busy";
    EFBIG = 27, "File too large";
    ENOSPC = 28, "No space left on device";
    ESPIPE = 29, "Illegal seek";
    EROFS = 30, "Read-only file system";
    EMLINK = 31, "Too many links";
    EPIPE = 32, "Broken pipe";
    EDOM = 33, "Numerical argument out of domain";
    ERANGE = 34, "Numerical result out of range";
    EDEADLK = 35, "Resource deadlock avoided";
    ENAMETOOLONG = 36, "File name too long";
    ENOLCK = 37, "No locks available";
    ENOSYS = 38, "Function not implemented";
    ENOTEMPTY = 39, "Directory not empty";
    ELOOP = 40, "Too many levels of symbolic links";
    ENODATA = 61, "No data available";
    ETIME = 62, "Timer expired";
    EPROTO = 71, "Protocol error";
    EOVERFLOW = 75, "Value too large for defined data type";
    EILSEQ = 84, "Invalid or incomplete multibyte or wide character";
    ENOTSOCK = 88, "Socket operation on non-socket";
    EDESTADDRREQ = 89, "Destination address required";
    ELIBBAD = 80, "Accessing a corrupted shared library";
    EMSGSIZE = 90, "Message too long";
    EPROTOTYPE = 91, "Protocol wrong type for socket";
    ENOPROTOOPT = 92, "Protocol not available";
    EPROTONOSUPPORT = 93, "Protocol not supported";
    ESOCKTNOSUPPORT = 94, "Socket type not supported";
    EOPNOTSUPP = 95, "Operation not supported";
    EPFNOSUPPORT = 96, "Protocol family not supported";
    EAFNOSUPPORT = 97, "Address family not supported by protocol";
    EADDRINUSE = 98, "Address already in use";
    EADDRNOTAVAIL = 99, "Cannot assign requested address";
    ENETDOWN = 100, "Network is down";
    ENETUNREACH = 101, "Network is unreachable";
    ENETRESET = 102, "Network dropped connection on reset";
    ECONNABORTED = 103, "Software caused connection abort";
    ECONNRESET = 104, "Connection reset by peer";
    ENOBUFS = 105, "No buffer space available";
    EISCONN = 106, "Transport endpoint is already connected";
    ENOTCONN = 107, "Transport endpoint is not connected";
    ESHUTDOWN = 108, "Cannot send after transport endpoint shutdown";
    ETIMEDOUT = 110, "Connection timed out";
    ENOMEDIUM = 123, "No medium found";
    ECONNREFUSED = 111, "Connection refused";
    EHOSTDOWN = 112, "Host is down";
    EHOSTUNREACH = 113, "No route to host";
    EALREADY = 114, "Operation already in progress";
    EINPROGRESS = 115, "Operation now in progress";
    ECANCELED = 125, "Operation canceled";
}

impl core::fmt::Display for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.message())
    }
}
