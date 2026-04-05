#include <Block.h>
#include <dispatch/dispatch.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <limits.h>
#include <mach/mach.h>
#include <mach-o/dyld.h>
#include <mach-o/loader.h>
#include <mach-o/nlist.h>
#include <objc/message.h>
#include <objc/runtime.h>
#include <pwd.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>
#include <xpc/xpc.h>

#define VIXEN_APP_GROUP_ID "B2N6FSRTPV.vixenfs"
#define VIXEN_XPC_SPY_LOG_NAME "vixenfs-xpc-spy.log"

#ifdef __LP64__
typedef struct mach_header_64 vxn_mach_header_t;
typedef struct segment_command_64 vxn_segment_command_t;
typedef struct section_64 vxn_section_t;
typedef struct nlist_64 vxn_nlist_t;
#define VXN_LC_SEGMENT_ARCH_DEPENDENT LC_SEGMENT_64
#else
typedef struct mach_header vxn_mach_header_t;
typedef struct segment_command vxn_segment_command_t;
typedef struct section vxn_section_t;
typedef struct nlist vxn_nlist_t;
#define VXN_LC_SEGMENT_ARCH_DEPENDENT LC_SEGMENT
#endif

#ifndef SEG_DATA_CONST
#define SEG_DATA_CONST "__DATA_CONST"
#endif

typedef void (^vxn_xpc_handler_t)(xpc_object_t object);
typedef void (*vxn_set_event_handler_fn)(xpc_connection_t connection, vxn_xpc_handler_t handler);
typedef void (*vxn_activate_fn)(xpc_connection_t connection);
typedef void (*vxn_resume_fn)(xpc_connection_t connection);
typedef void (*vxn_send_message_fn)(xpc_connection_t connection, xpc_object_t message);
typedef void (^vxn_xpc_reply_handler_t)(xpc_object_t object);
typedef void (*vxn_send_message_with_reply_fn)(
    xpc_connection_t connection,
    xpc_object_t message,
    dispatch_queue_t queue,
    vxn_xpc_reply_handler_t handler
);
typedef xpc_connection_t (*vxn_create_from_endpoint_fn)(xpc_endpoint_t endpoint);
typedef xpc_connection_t (*vxn_create_mach_service_fn)(
    const char *name,
    dispatch_queue_t targetq,
    uint64_t flags
);

struct rebinding {
    const char *name;
    void *replacement;
    void **replaced;
};

struct rebindings_entry {
    struct rebinding *rebindings;
    size_t rebindings_nel;
    struct rebindings_entry *next;
};

static struct rebindings_entry *vxn_rebindings_head;

static vxn_set_event_handler_fn vxn_original_set_event_handler;
static vxn_activate_fn vxn_original_activate;
static vxn_resume_fn vxn_original_resume;
static vxn_send_message_fn vxn_original_send_message;
static vxn_send_message_with_reply_fn vxn_original_send_message_with_reply;
static vxn_create_from_endpoint_fn vxn_original_create_from_endpoint;
static vxn_create_mach_service_fn vxn_original_create_mach_service;
static bool vxn_did_swizzle_nsxpc;

typedef id (*vxn_objc_msgsend_id)(id, SEL, ...);
typedef const char *(*vxn_objc_msgsend_cstr)(id, SEL, ...);
typedef void (*vxn_objc_msgsend_void)(id, SEL, ...);
typedef BOOL (*vxn_objc_msgsend_bool)(id, SEL, ...);

static IMP vxn_nsxpc_connection_resume_imp;
static IMP vxn_nsxpc_connection_set_exported_interface_imp;
static IMP vxn_nsxpc_connection_set_remote_interface_imp;
static IMP vxn_nsxpc_connection_set_exported_object_imp;
static IMP vxn_nsxpc_connection_init_with_listener_endpoint_imp;
static IMP vxn_nsxpc_connection_init_with_mach_service_imp;
static IMP vxn_nsxpc_listener_resume_imp;
static IMP vxn_nsxpc_listener_set_delegate_imp;
static IMP vxn_nsxpc_listener_init_with_mach_service_imp;
static IMP vxn_listener_should_accept_new_connection_imp;

static int vxn_prepend_rebindings(
    struct rebindings_entry **rebindings_head,
    struct rebinding rebindings[],
    size_t nel
) {
    struct rebindings_entry *new_entry =
        (struct rebindings_entry *)malloc(sizeof(struct rebindings_entry));
    if (new_entry == NULL) {
        return -1;
    }

    new_entry->rebindings = (struct rebinding *)malloc(sizeof(struct rebinding) * nel);
    if (new_entry->rebindings == NULL) {
        free(new_entry);
        return -1;
    }

    memcpy(new_entry->rebindings, rebindings, sizeof(struct rebinding) * nel);
    new_entry->rebindings_nel = nel;
    new_entry->next = *rebindings_head;
    *rebindings_head = new_entry;
    return 0;
}

static void vxn_perform_rebinding_with_section(
    struct rebindings_entry *rebindings,
    vxn_section_t *section,
    intptr_t slide,
    vxn_nlist_t *symtab,
    char *strtab,
    uint32_t *indirect_symtab
) {
    uint32_t *indirect_symbol_indices = indirect_symtab + section->reserved1;
    void **indirect_symbol_bindings = (void **)((uintptr_t)slide + section->addr);

    for (uint i = 0; i < section->size / sizeof(void *); i++) {
        uint32_t symtab_index = indirect_symbol_indices[i];
        if (symtab_index == INDIRECT_SYMBOL_ABS || symtab_index == INDIRECT_SYMBOL_LOCAL ||
            symtab_index == (INDIRECT_SYMBOL_LOCAL | INDIRECT_SYMBOL_ABS)) {
            continue;
        }

        uint32_t strtab_offset = symtab[symtab_index].n_un.n_strx;
        char *symbol_name = strtab + strtab_offset;
        bool symbol_name_longer_than_1 = symbol_name[0] != '\0' && symbol_name[1] != '\0';
        struct rebindings_entry *cur = rebindings;

        while (cur != NULL) {
            for (uint j = 0; j < cur->rebindings_nel; j++) {
                if (!symbol_name_longer_than_1 ||
                    strcmp(&symbol_name[1], cur->rebindings[j].name) != 0) {
                    continue;
                }

                if (cur->rebindings[j].replaced != NULL &&
                    indirect_symbol_bindings[i] != cur->rebindings[j].replacement) {
                    *(cur->rebindings[j].replaced) = indirect_symbol_bindings[i];
                }

                kern_return_t err = vm_protect(
                    mach_task_self(),
                    (uintptr_t)indirect_symbol_bindings,
                    section->size,
                    0,
                    VM_PROT_READ | VM_PROT_WRITE | VM_PROT_COPY
                );
                if (err == KERN_SUCCESS) {
                    indirect_symbol_bindings[i] = cur->rebindings[j].replacement;
                }
                goto symbol_loop;
            }
            cur = cur->next;
        }
symbol_loop:;
    }
}

static void vxn_rebind_symbols_for_image(
    struct rebindings_entry *rebindings,
    const struct mach_header *header,
    intptr_t slide
) {
    Dl_info info;
    if (dladdr(header, &info) == 0) {
        return;
    }

    vxn_segment_command_t *cur_seg_cmd = NULL;
    vxn_segment_command_t *linkedit_segment = NULL;
    struct symtab_command *symtab_cmd = NULL;
    struct dysymtab_command *dysymtab_cmd = NULL;

    uintptr_t cur = (uintptr_t)header + sizeof(vxn_mach_header_t);
    for (uint i = 0; i < ((const vxn_mach_header_t *)header)->ncmds; i++, cur += cur_seg_cmd->cmdsize) {
        cur_seg_cmd = (vxn_segment_command_t *)cur;
        if (cur_seg_cmd->cmd == VXN_LC_SEGMENT_ARCH_DEPENDENT) {
            if (strcmp(cur_seg_cmd->segname, SEG_LINKEDIT) == 0) {
                linkedit_segment = cur_seg_cmd;
            }
        } else if (cur_seg_cmd->cmd == LC_SYMTAB) {
            symtab_cmd = (struct symtab_command *)cur_seg_cmd;
        } else if (cur_seg_cmd->cmd == LC_DYSYMTAB) {
            dysymtab_cmd = (struct dysymtab_command *)cur_seg_cmd;
        }
    }

    if (symtab_cmd == NULL || dysymtab_cmd == NULL || linkedit_segment == NULL ||
        !dysymtab_cmd->nindirectsyms) {
        return;
    }

    uintptr_t linkedit_base =
        (uintptr_t)slide + linkedit_segment->vmaddr - linkedit_segment->fileoff;
    vxn_nlist_t *symtab = (vxn_nlist_t *)(linkedit_base + symtab_cmd->symoff);
    char *strtab = (char *)(linkedit_base + symtab_cmd->stroff);
    uint32_t *indirect_symtab = (uint32_t *)(linkedit_base + dysymtab_cmd->indirectsymoff);

    cur = (uintptr_t)header + sizeof(vxn_mach_header_t);
    for (uint i = 0; i < ((const vxn_mach_header_t *)header)->ncmds; i++, cur += cur_seg_cmd->cmdsize) {
        cur_seg_cmd = (vxn_segment_command_t *)cur;
        if (cur_seg_cmd->cmd != VXN_LC_SEGMENT_ARCH_DEPENDENT) {
            continue;
        }
        if (strcmp(cur_seg_cmd->segname, SEG_DATA) != 0 &&
            strcmp(cur_seg_cmd->segname, SEG_DATA_CONST) != 0) {
            continue;
        }

        for (uint j = 0; j < cur_seg_cmd->nsects; j++) {
            vxn_section_t *sect = (vxn_section_t *)(cur + sizeof(vxn_segment_command_t)) + j;
            uint32_t type = sect->flags & SECTION_TYPE;
            if (type == S_LAZY_SYMBOL_POINTERS || type == S_NON_LAZY_SYMBOL_POINTERS) {
                vxn_perform_rebinding_with_section(
                    rebindings,
                    sect,
                    slide,
                    symtab,
                    strtab,
                    indirect_symtab
                );
            }
        }
    }
}

static void vxn_rebind_symbols_for_loaded_image(const struct mach_header *header, intptr_t slide) {
    vxn_rebind_symbols_for_image(vxn_rebindings_head, header, slide);
}

static int vxn_rebind_symbols(struct rebinding rebindings[], size_t rebindings_nel) {
    int retval = vxn_prepend_rebindings(&vxn_rebindings_head, rebindings, rebindings_nel);
    if (retval < 0) {
        return retval;
    }

    if (vxn_rebindings_head->next == NULL) {
        _dyld_register_func_for_add_image(vxn_rebind_symbols_for_loaded_image);
    } else {
        uint32_t count = _dyld_image_count();
        for (uint32_t i = 0; i < count; i++) {
            vxn_rebind_symbols_for_loaded_image(
                _dyld_get_image_header(i),
                _dyld_get_image_vmaddr_slide(i)
            );
        }
    }

    return retval;
}

static const char *vxn_connection_name(xpc_connection_t connection) {
    const char *name = xpc_connection_get_name(connection);
    return name ? name : "<anonymous>";
}

static const char *vxn_log_path(void) {
    static char path[PATH_MAX];
    static dispatch_once_t once_token;
    dispatch_once(&once_token, ^{
        const char *home = NULL;
        struct passwd *pwd = getpwuid(getuid());
        if (pwd != NULL && pwd->pw_dir != NULL && pwd->pw_dir[0] != '\0') {
            home = pwd->pw_dir;
        }
        if (home == NULL || home[0] == '\0') {
            home = getenv("HOME");
        }
        if (home == NULL || home[0] == '\0') {
            home = ".";
        }
        snprintf(
            path,
            sizeof(path),
            "%s/Library/Group Containers/%s/Logs/%s",
            home,
            VIXEN_APP_GROUP_ID,
            VIXEN_XPC_SPY_LOG_NAME
        );
    });
    return path;
}

static void vxn_mkdir_p(const char *dir_path) {
    char tmp[PATH_MAX];
    size_t len = strlcpy(tmp, dir_path, sizeof(tmp));
    if (len == 0 || len >= sizeof(tmp)) {
        return;
    }

    for (char *cursor = tmp + 1; *cursor != '\0'; cursor++) {
        if (*cursor != '/') {
            continue;
        }
        *cursor = '\0';
        (void)mkdir(tmp, 0755);
        *cursor = '/';
    }
    (void)mkdir(tmp, 0755);
}

static void vxn_log(const char *format, ...) {
    const char *path = vxn_log_path();
    char dir_path[PATH_MAX];
    if (strlcpy(dir_path, path, sizeof(dir_path)) >= sizeof(dir_path)) {
        return;
    }
    char *slash = strrchr(dir_path, '/');
    if (slash != NULL) {
        *slash = '\0';
        vxn_mkdir_p(dir_path);
    }

    int fd = open(path, O_CREAT | O_APPEND | O_WRONLY, 0644);
    if (fd < 0) {
        return;
    }

    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    dprintf(fd, "[%lld.%03d] [xpc-spy] ", (long long)ts.tv_sec, (int)(ts.tv_nsec / 1000000));

    va_list args;
    va_start(args, format);
    vdprintf(fd, format, args);
    va_end(args);

    dprintf(fd, "\n");
    close(fd);
}

static char *vxn_copy_description(xpc_object_t object) {
    if (object == NULL) {
        return strdup("<null>");
    }
    char *desc = xpc_copy_description(object);
    if (desc == NULL) {
        return strdup("<description-null>");
    }
    return desc;
}

static char *vxn_copy_nsobject_description(id object) {
    if (object == nil) {
        return strdup("<nil>");
    }

    const char *class_name = object_getClassName(object);
    SEL description_sel = sel_registerName("description");
    SEL utf8_sel = sel_registerName("UTF8String");
    id description = ((vxn_objc_msgsend_id)objc_msgSend)(object, description_sel);
    const char *utf8 = NULL;
    if (description != nil) {
        utf8 = ((vxn_objc_msgsend_cstr)objc_msgSend)(description, utf8_sel);
    }

    char buffer[2048];
    snprintf(
        buffer,
        sizeof(buffer),
        "<%s:%p> %s",
        class_name ? class_name : "<unknown-class>",
        (void *)object,
        utf8 ? utf8 : "<no-description>"
    );
    return strdup(buffer);
}

static void vxn_swizzle_method(Class cls, SEL sel, IMP replacement, IMP *original_store) {
    if (cls == Nil || sel == NULL) {
        return;
    }
    Method method = class_getInstanceMethod(cls, sel);
    if (method == NULL) {
        return;
    }

    IMP current = method_getImplementation(method);
    if (current == replacement) {
        return;
    }

    if (original_store != NULL && *original_store == NULL) {
        *original_store = current;
    }
    method_setImplementation(method, replacement);
}

static void vxn_maybe_swizzle_listener_delegate(id delegate);

static void vxn_xpc_connection_set_event_handler(
    xpc_connection_t connection,
    vxn_xpc_handler_t handler
) {
    vxn_log(
        "set_event_handler connection=%p connection_name=%s handler=%p",
        connection,
        connection ? vxn_connection_name(connection) : "<null>",
        handler
    );

    if (handler == NULL) {
        if (vxn_original_set_event_handler != NULL) {
            vxn_original_set_event_handler(connection, handler);
        }
        return;
    }

    vxn_xpc_handler_t copied_handler = Block_copy(handler);
    vxn_xpc_handler_t wrapped = ^(xpc_object_t event) {
        char *desc = vxn_copy_description(event);
        const char *event_type = "<unknown>";
        xpc_type_t type = xpc_get_type(event);
        if (type == XPC_TYPE_DICTIONARY) {
            event_type = "dictionary";
        } else if (type == XPC_TYPE_CONNECTION) {
            event_type = "connection";
        } else if (type == XPC_TYPE_ERROR) {
            event_type = "error";
        }
        vxn_log(
            "incoming_event connection=%p connection_name=%s type=%s desc=%s",
            connection,
            connection ? vxn_connection_name(connection) : "<null>",
            event_type,
            desc
        );
        free(desc);
        copied_handler(event);
    };

    if (vxn_original_set_event_handler != NULL) {
        vxn_original_set_event_handler(connection, Block_copy(wrapped));
    }
}

static void vxn_xpc_connection_activate(xpc_connection_t connection) {
    vxn_log(
        "activate connection=%p connection_name=%s",
        connection,
        connection ? vxn_connection_name(connection) : "<null>"
    );
    if (vxn_original_activate != NULL) {
        vxn_original_activate(connection);
    }
}

static void vxn_xpc_connection_resume(xpc_connection_t connection) {
    vxn_log(
        "resume connection=%p connection_name=%s",
        connection,
        connection ? vxn_connection_name(connection) : "<null>"
    );
    if (vxn_original_resume != NULL) {
        vxn_original_resume(connection);
    }
}

static void vxn_xpc_connection_send_message(xpc_connection_t connection, xpc_object_t message) {
    char *desc = vxn_copy_description(message);
    vxn_log(
        "send_message connection=%p connection_name=%s desc=%s",
        connection,
        connection ? vxn_connection_name(connection) : "<null>",
        desc
    );
    free(desc);
    if (vxn_original_send_message != NULL) {
        vxn_original_send_message(connection, message);
    }
}

static void vxn_xpc_connection_send_message_with_reply(
    xpc_connection_t connection,
    xpc_object_t message,
    dispatch_queue_t queue,
    vxn_xpc_reply_handler_t handler
) {
    char *desc = vxn_copy_description(message);
    vxn_log(
        "send_message_with_reply connection=%p connection_name=%s queue=%p handler=%p desc=%s",
        connection,
        connection ? vxn_connection_name(connection) : "<null>",
        queue,
        handler,
        desc
    );
    free(desc);

    if (handler == NULL) {
        if (vxn_original_send_message_with_reply != NULL) {
            vxn_original_send_message_with_reply(connection, message, queue, handler);
        }
        return;
    }

    vxn_xpc_reply_handler_t copied_handler = Block_copy(handler);
    vxn_xpc_reply_handler_t wrapped = ^(xpc_object_t reply) {
        char *reply_desc = vxn_copy_description(reply);
        vxn_log(
            "reply connection=%p connection_name=%s desc=%s",
            connection,
            connection ? vxn_connection_name(connection) : "<null>",
            reply_desc
        );
        free(reply_desc);
        copied_handler(reply);
    };

    if (vxn_original_send_message_with_reply != NULL) {
        vxn_original_send_message_with_reply(connection, message, queue, Block_copy(wrapped));
    }
}

static xpc_connection_t vxn_xpc_connection_create_from_endpoint(xpc_endpoint_t endpoint) {
    char *desc = vxn_copy_description(endpoint);
    vxn_log("create_from_endpoint endpoint=%p desc=%s", endpoint, desc);
    free(desc);

    if (vxn_original_create_from_endpoint == NULL) {
        return NULL;
    }

    xpc_connection_t connection = vxn_original_create_from_endpoint(endpoint);
    vxn_log(
        "create_from_endpoint.result connection=%p connection_name=%s",
        connection,
        connection ? vxn_connection_name(connection) : "<null>"
    );
    return connection;
}

static xpc_connection_t vxn_xpc_connection_create_mach_service(
    const char *name,
    dispatch_queue_t targetq,
    uint64_t flags
) {
    vxn_log(
        "create_mach_service name=%s queue=%p flags=%llu",
        name ? name : "<null>",
        targetq,
        (unsigned long long)flags
    );

    if (vxn_original_create_mach_service == NULL) {
        return NULL;
    }

    xpc_connection_t connection = vxn_original_create_mach_service(name, targetq, flags);
    vxn_log(
        "create_mach_service.result connection=%p connection_name=%s",
        connection,
        connection ? vxn_connection_name(connection) : "<null>"
    );
    return connection;
}

static void vxn_nsxpc_connection_resume(id self, SEL _cmd) {
    char *self_desc = vxn_copy_nsobject_description(self);
    vxn_log("NSXPCConnection resume self=%s", self_desc);
    free(self_desc);

    if (vxn_nsxpc_connection_resume_imp != NULL) {
        ((void (*)(id, SEL))vxn_nsxpc_connection_resume_imp)(self, _cmd);
    }
}

static void vxn_nsxpc_connection_set_exported_interface(id self, SEL _cmd, id interface) {
    char *self_desc = vxn_copy_nsobject_description(self);
    char *iface_desc = vxn_copy_nsobject_description(interface);
    vxn_log(
        "NSXPCConnection setExportedInterface self=%s interface=%s",
        self_desc,
        iface_desc
    );
    free(self_desc);
    free(iface_desc);

    if (vxn_nsxpc_connection_set_exported_interface_imp != NULL) {
        ((void (*)(id, SEL, id))vxn_nsxpc_connection_set_exported_interface_imp)(
            self,
            _cmd,
            interface
        );
    }
}

static void vxn_nsxpc_connection_set_remote_object_interface(id self, SEL _cmd, id interface) {
    char *self_desc = vxn_copy_nsobject_description(self);
    char *iface_desc = vxn_copy_nsobject_description(interface);
    vxn_log(
        "NSXPCConnection setRemoteObjectInterface self=%s interface=%s",
        self_desc,
        iface_desc
    );
    free(self_desc);
    free(iface_desc);

    if (vxn_nsxpc_connection_set_remote_interface_imp != NULL) {
        ((void (*)(id, SEL, id))vxn_nsxpc_connection_set_remote_interface_imp)(
            self,
            _cmd,
            interface
        );
    }
}

static void vxn_nsxpc_connection_set_exported_object(id self, SEL _cmd, id object) {
    char *self_desc = vxn_copy_nsobject_description(self);
    char *obj_desc = vxn_copy_nsobject_description(object);
    vxn_log("NSXPCConnection setExportedObject self=%s object=%s", self_desc, obj_desc);
    free(self_desc);
    free(obj_desc);

    if (vxn_nsxpc_connection_set_exported_object_imp != NULL) {
        ((void (*)(id, SEL, id))vxn_nsxpc_connection_set_exported_object_imp)(self, _cmd, object);
    }
}

static id vxn_nsxpc_connection_init_with_listener_endpoint(id self, SEL _cmd, id endpoint) {
    char *endpoint_desc = vxn_copy_nsobject_description(endpoint);
    vxn_log(
        "NSXPCConnection initWithListenerEndpoint self=%p endpoint=%s",
        (void *)self,
        endpoint_desc
    );
    free(endpoint_desc);

    if (vxn_nsxpc_connection_init_with_listener_endpoint_imp == NULL) {
        return self;
    }

    id result = ((id (*)(id, SEL, id))vxn_nsxpc_connection_init_with_listener_endpoint_imp)(
        self,
        _cmd,
        endpoint
    );
    char *result_desc = vxn_copy_nsobject_description(result);
    vxn_log("NSXPCConnection initWithListenerEndpoint result=%s", result_desc);
    free(result_desc);
    return result;
}

static id vxn_nsxpc_connection_init_with_mach_service(
    id self,
    SEL _cmd,
    id service_name,
    unsigned long long options
) {
    char *name_desc = vxn_copy_nsobject_description(service_name);
    vxn_log(
        "NSXPCConnection initWithMachServiceName self=%p service=%s options=%llu",
        (void *)self,
        name_desc,
        options
    );
    free(name_desc);

    if (vxn_nsxpc_connection_init_with_mach_service_imp == NULL) {
        return self;
    }

    id result = ((id (*)(id, SEL, id, unsigned long long))
                     vxn_nsxpc_connection_init_with_mach_service_imp)(
        self,
        _cmd,
        service_name,
        options
    );
    char *result_desc = vxn_copy_nsobject_description(result);
    vxn_log("NSXPCConnection initWithMachServiceName result=%s", result_desc);
    free(result_desc);
    return result;
}

static void vxn_nsxpc_listener_resume(id self, SEL _cmd) {
    char *self_desc = vxn_copy_nsobject_description(self);
    vxn_log("NSXPCListener resume self=%s", self_desc);
    free(self_desc);

    if (vxn_nsxpc_listener_resume_imp != NULL) {
        ((void (*)(id, SEL))vxn_nsxpc_listener_resume_imp)(self, _cmd);
    }
}

static void vxn_nsxpc_listener_set_delegate(id self, SEL _cmd, id delegate) {
    char *self_desc = vxn_copy_nsobject_description(self);
    char *delegate_desc = vxn_copy_nsobject_description(delegate);
    vxn_log("NSXPCListener setDelegate self=%s delegate=%s", self_desc, delegate_desc);
    free(self_desc);
    free(delegate_desc);

    vxn_maybe_swizzle_listener_delegate(delegate);

    if (vxn_nsxpc_listener_set_delegate_imp != NULL) {
        ((void (*)(id, SEL, id))vxn_nsxpc_listener_set_delegate_imp)(self, _cmd, delegate);
    }
}

static id vxn_nsxpc_listener_init_with_mach_service(id self, SEL _cmd, id service_name) {
    char *name_desc = vxn_copy_nsobject_description(service_name);
    vxn_log(
        "NSXPCListener initWithMachServiceName self=%p service=%s",
        (void *)self,
        name_desc
    );
    free(name_desc);

    if (vxn_nsxpc_listener_init_with_mach_service_imp == NULL) {
        return self;
    }

    id result = ((id (*)(id, SEL, id))vxn_nsxpc_listener_init_with_mach_service_imp)(
        self,
        _cmd,
        service_name
    );
    char *result_desc = vxn_copy_nsobject_description(result);
    vxn_log("NSXPCListener initWithMachServiceName result=%s", result_desc);
    free(result_desc);
    return result;
}

static BOOL vxn_listener_should_accept_new_connection(
    id self,
    SEL _cmd,
    id listener,
    id connection
) {
    char *self_desc = vxn_copy_nsobject_description(self);
    char *listener_desc = vxn_copy_nsobject_description(listener);
    char *connection_desc = vxn_copy_nsobject_description(connection);
    vxn_log(
        "listener:shouldAcceptNewConnection: delegate=%s listener=%s connection=%s",
        self_desc,
        listener_desc,
        connection_desc
    );
    free(self_desc);
    free(listener_desc);
    free(connection_desc);

    if (vxn_listener_should_accept_new_connection_imp == NULL) {
        return YES;
    }

    return ((BOOL (*)(id, SEL, id, id))vxn_listener_should_accept_new_connection_imp)(
        self,
        _cmd,
        listener,
        connection
    );
}

static void vxn_maybe_swizzle_listener_delegate(id delegate) {
    if (delegate == nil) {
        return;
    }
    Class cls = object_getClass(delegate);
    vxn_swizzle_method(
        cls,
        sel_registerName("listener:shouldAcceptNewConnection:"),
        (IMP)vxn_listener_should_accept_new_connection,
        &vxn_listener_should_accept_new_connection_imp
    );
}

static void vxn_swizzle_nsxpc_methods(void) {
    if (vxn_did_swizzle_nsxpc) {
        return;
    }
    vxn_did_swizzle_nsxpc = true;

    Class nsxpc_connection = objc_getClass("NSXPCConnection");
    Class nsxpc_listener = objc_getClass("NSXPCListener");

    vxn_swizzle_method(
        nsxpc_connection,
        sel_registerName("resume"),
        (IMP)vxn_nsxpc_connection_resume,
        &vxn_nsxpc_connection_resume_imp
    );
    vxn_swizzle_method(
        nsxpc_connection,
        sel_registerName("setExportedInterface:"),
        (IMP)vxn_nsxpc_connection_set_exported_interface,
        &vxn_nsxpc_connection_set_exported_interface_imp
    );
    vxn_swizzle_method(
        nsxpc_connection,
        sel_registerName("setRemoteObjectInterface:"),
        (IMP)vxn_nsxpc_connection_set_remote_object_interface,
        &vxn_nsxpc_connection_set_remote_interface_imp
    );
    vxn_swizzle_method(
        nsxpc_connection,
        sel_registerName("setExportedObject:"),
        (IMP)vxn_nsxpc_connection_set_exported_object,
        &vxn_nsxpc_connection_set_exported_object_imp
    );
    vxn_swizzle_method(
        nsxpc_connection,
        sel_registerName("initWithListenerEndpoint:"),
        (IMP)vxn_nsxpc_connection_init_with_listener_endpoint,
        &vxn_nsxpc_connection_init_with_listener_endpoint_imp
    );
    vxn_swizzle_method(
        nsxpc_connection,
        sel_registerName("initWithMachServiceName:options:"),
        (IMP)vxn_nsxpc_connection_init_with_mach_service,
        &vxn_nsxpc_connection_init_with_mach_service_imp
    );

    vxn_swizzle_method(
        nsxpc_listener,
        sel_registerName("resume"),
        (IMP)vxn_nsxpc_listener_resume,
        &vxn_nsxpc_listener_resume_imp
    );
    vxn_swizzle_method(
        nsxpc_listener,
        sel_registerName("setDelegate:"),
        (IMP)vxn_nsxpc_listener_set_delegate,
        &vxn_nsxpc_listener_set_delegate_imp
    );
    vxn_swizzle_method(
        nsxpc_listener,
        sel_registerName("initWithMachServiceName:"),
        (IMP)vxn_nsxpc_listener_init_with_mach_service,
        &vxn_nsxpc_listener_init_with_mach_service_imp
    );
}

__attribute__((constructor)) static void vxn_xpc_spy_init(void) {
    struct rebinding rebindings[] = {
        {
            .name = "xpc_connection_set_event_handler",
            .replacement = (void *)vxn_xpc_connection_set_event_handler,
            .replaced = (void **)&vxn_original_set_event_handler,
        },
        {
            .name = "xpc_connection_activate",
            .replacement = (void *)vxn_xpc_connection_activate,
            .replaced = (void **)&vxn_original_activate,
        },
        {
            .name = "xpc_connection_resume",
            .replacement = (void *)vxn_xpc_connection_resume,
            .replaced = (void **)&vxn_original_resume,
        },
        {
            .name = "xpc_connection_send_message",
            .replacement = (void *)vxn_xpc_connection_send_message,
            .replaced = (void **)&vxn_original_send_message,
        },
        {
            .name = "xpc_connection_send_message_with_reply",
            .replacement = (void *)vxn_xpc_connection_send_message_with_reply,
            .replaced = (void **)&vxn_original_send_message_with_reply,
        },
        {
            .name = "xpc_connection_create_from_endpoint",
            .replacement = (void *)vxn_xpc_connection_create_from_endpoint,
            .replaced = (void **)&vxn_original_create_from_endpoint,
        },
        {
            .name = "xpc_connection_create_mach_service",
            .replacement = (void *)vxn_xpc_connection_create_mach_service,
            .replaced = (void **)&vxn_original_create_mach_service,
        },
    };
    (void)vxn_rebind_symbols(rebindings, sizeof(rebindings) / sizeof(rebindings[0]));
    vxn_swizzle_nsxpc_methods();
    vxn_log(
        "xpc spy init pid=%d rebound set_handler=%p activate=%p resume=%p send=%p send_reply=%p create_ep=%p create_mach=%p swizzled_nsxpc=%d",
        getpid(),
        vxn_original_set_event_handler,
        vxn_original_activate,
        vxn_original_resume,
        vxn_original_send_message,
        vxn_original_send_message_with_reply,
        vxn_original_create_from_endpoint,
        vxn_original_create_mach_service,
        vxn_did_swizzle_nsxpc ? 1 : 0
    );
}
