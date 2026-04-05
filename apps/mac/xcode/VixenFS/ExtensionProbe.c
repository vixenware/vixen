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
#define VIXEN_EXTENSION_PROBE_LOG_NAME "vixenfs-extension-probe.log"

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

struct vxn_class_imp_entry {
    Class cls;
    IMP imp;
    struct vxn_class_imp_entry *next;
};

struct vxn_seen_class_entry {
    Class cls;
    struct vxn_seen_class_entry *next;
};

struct vxn_method_imp_entry {
    Class cls;
    SEL sel;
    IMP imp;
    struct vxn_method_imp_entry *next;
};

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
static IMP vxn_nsinvocation_invoke_with_target_imp;
static IMP vxn_nsxpcinterface_set_classes_imp;
static IMP vxn_nsxpcinterface_set_interface_imp;
static IMP vxn_nsxpcinterface_set_reply_block_signature_imp;
static struct vxn_class_imp_entry *vxn_listener_delegate_imps;
static struct vxn_seen_class_entry *vxn_logged_exported_object_classes;
static struct vxn_method_imp_entry *vxn_exported_method_imps;

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
            VIXEN_EXTENSION_PROBE_LOG_NAME
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
    dprintf(
        fd,
        "[%lld.%03d] [extension-probe] ",
        (long long)ts.tv_sec,
        (int)(ts.tv_nsec / 1000000)
    );

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

static IMP vxn_lookup_method_imp(struct vxn_method_imp_entry *head, Class cls, SEL sel) {
    for (Class cursor = cls; cursor != Nil; cursor = class_getSuperclass(cursor)) {
        for (struct vxn_method_imp_entry *entry = head; entry != NULL; entry = entry->next) {
            if (entry->cls == cursor && entry->sel == sel) {
                return entry->imp;
            }
        }
    }
    return NULL;
}

static void vxn_store_method_imp(
    struct vxn_method_imp_entry **head,
    Class cls,
    SEL sel,
    IMP imp
) {
    for (struct vxn_method_imp_entry *entry = *head; entry != NULL; entry = entry->next) {
        if (entry->cls == cls && entry->sel == sel) {
            entry->imp = imp;
            return;
        }
    }

    struct vxn_method_imp_entry *entry =
        (struct vxn_method_imp_entry *)calloc(1, sizeof(struct vxn_method_imp_entry));
    if (entry == NULL) {
        return;
    }
    entry->cls = cls;
    entry->sel = sel;
    entry->imp = imp;
    entry->next = *head;
    *head = entry;
}

static IMP vxn_lookup_class_imp(struct vxn_class_imp_entry *head, Class cls) {
    for (Class cursor = cls; cursor != Nil; cursor = class_getSuperclass(cursor)) {
        for (struct vxn_class_imp_entry *entry = head; entry != NULL; entry = entry->next) {
            if (entry->cls == cursor) {
                return entry->imp;
            }
        }
    }
    return NULL;
}

static void vxn_store_class_imp(struct vxn_class_imp_entry **head, Class cls, IMP imp) {
    for (struct vxn_class_imp_entry *entry = *head; entry != NULL; entry = entry->next) {
        if (entry->cls == cls) {
            entry->imp = imp;
            return;
        }
    }

    struct vxn_class_imp_entry *entry =
        (struct vxn_class_imp_entry *)calloc(1, sizeof(struct vxn_class_imp_entry));
    if (entry == NULL) {
        return;
    }
    entry->cls = cls;
    entry->imp = imp;
    entry->next = *head;
    *head = entry;
}

static bool vxn_mark_class_seen(struct vxn_seen_class_entry **head, Class cls) {
    for (struct vxn_seen_class_entry *entry = *head; entry != NULL; entry = entry->next) {
        if (entry->cls == cls) {
            return false;
        }
    }

    struct vxn_seen_class_entry *entry =
        (struct vxn_seen_class_entry *)calloc(1, sizeof(struct vxn_seen_class_entry));
    if (entry == NULL) {
        return false;
    }
    entry->cls = cls;
    entry->next = *head;
    *head = entry;
    return true;
}

static void vxn_maybe_swizzle_listener_delegate(id delegate);
static void vxn_maybe_swizzle_exported_object(id object);

static void vxn_log_method_list_for_object_once(const char *label, id object) {
    if (object == nil) {
        return;
    }

    Class cls = object_getClass(object);
    if (cls == Nil || !vxn_mark_class_seen(&vxn_logged_exported_object_classes, cls)) {
        return;
    }

    unsigned int method_count = 0;
    Method *methods = class_copyMethodList(cls, &method_count);
    if (methods == NULL) {
        vxn_log(
            "class_methods label=%s class=%s count=0",
            label,
            class_getName(cls)
        );
        return;
    }

    vxn_log(
        "class_methods_begin label=%s class=%s count=%u",
        label,
        class_getName(cls),
        method_count
    );
    for (unsigned int i = 0; i < method_count; i++) {
        SEL method_sel = method_getName(methods[i]);
        const char *types = method_getTypeEncoding(methods[i]);
        vxn_log(
            "class_method label=%s class=%s selector=%s types=%s",
            label,
            class_getName(cls),
            method_sel ? sel_getName(method_sel) : "<null>",
            types ? types : "<null>"
        );
    }
    vxn_log("class_methods_end label=%s class=%s", label, class_getName(cls));
    free(methods);
}

static void vxn_log_exported_call1(const char *label, id self, SEL _cmd, id a1) {
    char *self_desc = vxn_copy_nsobject_description(self);
    char *a1_desc = vxn_copy_nsobject_description(a1);
    vxn_log(
        "%s self=%s selector=%s arg1=%s",
        label,
        self_desc,
        sel_getName(_cmd),
        a1_desc
    );
    free(self_desc);
    free(a1_desc);
}

static void vxn_log_exported_call2(const char *label, id self, SEL _cmd, id a1, id a2) {
    char *self_desc = vxn_copy_nsobject_description(self);
    char *a1_desc = vxn_copy_nsobject_description(a1);
    char *a2_desc = vxn_copy_nsobject_description(a2);
    vxn_log(
        "%s self=%s selector=%s arg1=%s arg2=%s",
        label,
        self_desc,
        sel_getName(_cmd),
        a1_desc,
        a2_desc
    );
    free(self_desc);
    free(a1_desc);
    free(a2_desc);
}

static void vxn_log_exported_call3(
    const char *label,
    id self,
    SEL _cmd,
    id a1,
    id a2,
    id a3
) {
    char *self_desc = vxn_copy_nsobject_description(self);
    char *a1_desc = vxn_copy_nsobject_description(a1);
    char *a2_desc = vxn_copy_nsobject_description(a2);
    char *a3_desc = vxn_copy_nsobject_description(a3);
    vxn_log(
        "%s self=%s selector=%s arg1=%s arg2=%s arg3=%s",
        label,
        self_desc,
        sel_getName(_cmd),
        a1_desc,
        a2_desc,
        a3_desc
    );
    free(self_desc);
    free(a1_desc);
    free(a2_desc);
    free(a3_desc);
}

static void vxn_log_connection_state(const char *label, id connection) {
    if (connection == nil) {
        return;
    }

    SEL exported_interface_sel = sel_registerName("exportedInterface");
    SEL remote_interface_sel = sel_registerName("remoteObjectInterface");
    SEL exported_object_sel = sel_registerName("exportedObject");

    id exported_interface = nil;
    id remote_interface = nil;
    id exported_object = nil;

    if (class_respondsToSelector(object_getClass(connection), exported_interface_sel)) {
        exported_interface = ((vxn_objc_msgsend_id)objc_msgSend)(connection, exported_interface_sel);
    }
    if (class_respondsToSelector(object_getClass(connection), remote_interface_sel)) {
        remote_interface = ((vxn_objc_msgsend_id)objc_msgSend)(connection, remote_interface_sel);
    }
    if (class_respondsToSelector(object_getClass(connection), exported_object_sel)) {
        exported_object = ((vxn_objc_msgsend_id)objc_msgSend)(connection, exported_object_sel);
    }

    char *connection_desc = vxn_copy_nsobject_description(connection);
    char *exported_interface_desc = vxn_copy_nsobject_description(exported_interface);
    char *remote_interface_desc = vxn_copy_nsobject_description(remote_interface);
    char *exported_object_desc = vxn_copy_nsobject_description(exported_object);
    vxn_log(
        "%s connection=%s exportedInterface=%s remoteInterface=%s exportedObject=%s",
        label,
        connection_desc,
        exported_interface_desc,
        remote_interface_desc,
        exported_object_desc
    );
    free(connection_desc);
    free(exported_interface_desc);
    free(remote_interface_desc);
    free(exported_object_desc);
}

static void vxn_swizzle_tracked_method(Class cls, const char *selector_name, IMP replacement) {
    if (cls == Nil) {
        return;
    }

    SEL sel = sel_registerName(selector_name);
    Method method = class_getInstanceMethod(cls, sel);
    if (method == NULL) {
        return;
    }

    IMP current = class_getMethodImplementation(cls, sel);
    if (current == replacement) {
        return;
    }

    vxn_store_method_imp(&vxn_exported_method_imps, cls, sel, current);
    const char *types = method_getTypeEncoding(method);
    if (!class_addMethod(cls, sel, replacement, types)) {
        method_setImplementation(method, replacement);
    }
}

static void vxn_fs_module_probe_resource(id self, SEL _cmd, id resource, id reply_handler) {
    vxn_log_exported_call2("exported.call", self, _cmd, resource, reply_handler);
    IMP original = vxn_lookup_method_imp(vxn_exported_method_imps, object_getClass(self), _cmd);
    if (original != NULL) {
        ((void (*)(id, SEL, id, id))original)(self, _cmd, resource, reply_handler);
    }
}

static void vxn_fs_module_load_resource(
    id self,
    SEL _cmd,
    id resource,
    id options,
    id reply_handler
) {
    vxn_log_exported_call3("exported.call", self, _cmd, resource, options, reply_handler);
    IMP original = vxn_lookup_method_imp(vxn_exported_method_imps, object_getClass(self), _cmd);
    if (original != NULL) {
        ((void (*)(id, SEL, id, id, id))original)(
            self,
            _cmd,
            resource,
            options,
            reply_handler
        );
    }
}

static void vxn_fs_module_activate_volume(
    id self,
    SEL _cmd,
    id volume,
    id resource,
    id options,
    id reply_handler
) {
    char *self_desc = vxn_copy_nsobject_description(self);
    char *volume_desc = vxn_copy_nsobject_description(volume);
    char *resource_desc = vxn_copy_nsobject_description(resource);
    char *options_desc = vxn_copy_nsobject_description(options);
    char *reply_desc = vxn_copy_nsobject_description(reply_handler);
    vxn_log(
        "exported.call self=%s selector=%s volume=%s resource=%s options=%s reply=%s",
        self_desc,
        sel_getName(_cmd),
        volume_desc,
        resource_desc,
        options_desc,
        reply_desc
    );
    free(self_desc);
    free(volume_desc);
    free(resource_desc);
    free(options_desc);
    free(reply_desc);

    IMP original = vxn_lookup_method_imp(vxn_exported_method_imps, object_getClass(self), _cmd);
    if (original != NULL) {
        ((void (*)(id, SEL, id, id, id, id))original)(
            self,
            _cmd,
            volume,
            resource,
            options,
            reply_handler
        );
    }
}

static void vxn_fs_module_get_volume_endpoint(
    id self,
    SEL _cmd,
    id volume,
    id reply_handler
) {
    vxn_log_exported_call2("exported.call", self, _cmd, volume, reply_handler);
    IMP original = vxn_lookup_method_imp(vxn_exported_method_imps, object_getClass(self), _cmd);
    if (original != NULL) {
        ((void (*)(id, SEL, id, id))original)(self, _cmd, volume, reply_handler);
    }
}

static void vxn_fs_volume_mount(id self, SEL _cmd, id options, id reply_handler) {
    vxn_log_exported_call2("exported.call", self, _cmd, options, reply_handler);
    IMP original = vxn_lookup_method_imp(vxn_exported_method_imps, object_getClass(self), _cmd);
    if (original != NULL) {
        ((void (*)(id, SEL, id, id))original)(self, _cmd, options, reply_handler);
    }
}

static void vxn_fs_volume_get_root_file_handle(id self, SEL _cmd, id reply_handler) {
    vxn_log_exported_call1("exported.call", self, _cmd, reply_handler);
    IMP original = vxn_lookup_method_imp(vxn_exported_method_imps, object_getClass(self), _cmd);
    if (original != NULL) {
        ((void (*)(id, SEL, id))original)(self, _cmd, reply_handler);
    }
}

static void vxn_fs_volume_get_standard_item_attributes_data(
    id self,
    SEL _cmd,
    id item,
    id reply_handler
) {
    vxn_log_exported_call2("exported.call", self, _cmd, item, reply_handler);
    IMP original = vxn_lookup_method_imp(vxn_exported_method_imps, object_getClass(self), _cmd);
    if (original != NULL) {
        ((void (*)(id, SEL, id, id))original)(self, _cmd, item, reply_handler);
    }
}

static void vxn_fs_volume_lookup_in(
    id self,
    SEL _cmd,
    id parent,
    id name,
    unsigned int flags,
    unsigned long long request_id,
    id reply_handler
) {
    char *self_desc = vxn_copy_nsobject_description(self);
    char *parent_desc = vxn_copy_nsobject_description(parent);
    char *name_desc = vxn_copy_nsobject_description(name);
    char *reply_desc = vxn_copy_nsobject_description(reply_handler);
    vxn_log(
        "exported.call self=%s selector=%s parent=%s name=%s flags=%u requestID=%llu reply=%s",
        self_desc,
        sel_getName(_cmd),
        parent_desc,
        name_desc,
        flags,
        request_id,
        reply_desc
    );
    free(self_desc);
    free(parent_desc);
    free(name_desc);
    free(reply_desc);

    IMP original = vxn_lookup_method_imp(vxn_exported_method_imps, object_getClass(self), _cmd);
    if (original != NULL) {
        ((void (*)(id, SEL, id, id, unsigned int, unsigned long long, id))original)(
            self,
            _cmd,
            parent,
            name,
            flags,
            request_id,
            reply_handler
        );
    }
}

static void vxn_fs_volume_open(
    id self,
    SEL _cmd,
    id item,
    int mode,
    unsigned long long request_id,
    id reply_handler
) {
    char *self_desc = vxn_copy_nsobject_description(self);
    char *item_desc = vxn_copy_nsobject_description(item);
    char *reply_desc = vxn_copy_nsobject_description(reply_handler);
    vxn_log(
        "exported.call self=%s selector=%s item=%s mode=%d requestID=%llu reply=%s",
        self_desc,
        sel_getName(_cmd),
        item_desc,
        mode,
        request_id,
        reply_desc
    );
    free(self_desc);
    free(item_desc);
    free(reply_desc);

    IMP original = vxn_lookup_method_imp(vxn_exported_method_imps, object_getClass(self), _cmd);
    if (original != NULL) {
        ((void (*)(id, SEL, id, int, unsigned long long, id))original)(
            self,
            _cmd,
            item,
            mode,
            request_id,
            reply_handler
        );
    }
}

static void vxn_fs_volume_read_from(
    id self,
    SEL _cmd,
    id item,
    unsigned long long offset,
    id buffer,
    unsigned long long request_id,
    id reply_handler
) {
    char *self_desc = vxn_copy_nsobject_description(self);
    char *item_desc = vxn_copy_nsobject_description(item);
    char *buffer_desc = vxn_copy_nsobject_description(buffer);
    char *reply_desc = vxn_copy_nsobject_description(reply_handler);
    vxn_log(
        "exported.call self=%s selector=%s item=%s offset=%llu buffer=%s requestID=%llu reply=%s",
        self_desc,
        sel_getName(_cmd),
        item_desc,
        offset,
        buffer_desc,
        request_id,
        reply_desc
    );
    free(self_desc);
    free(item_desc);
    free(buffer_desc);
    free(reply_desc);

    IMP original = vxn_lookup_method_imp(vxn_exported_method_imps, object_getClass(self), _cmd);
    if (original != NULL) {
        ((void (*)(id, SEL, id, unsigned long long, id, unsigned long long, id))original)(
            self,
            _cmd,
            item,
            offset,
            buffer,
            request_id,
            reply_handler
        );
    }
}

static void vxn_maybe_swizzle_exported_object(id object) {
    if (object == nil) {
        return;
    }

    Class cls = object_getClass(object);
    if (cls == Nil) {
        return;
    }

    const char *class_name = class_getName(cls);
    if (class_name == NULL) {
        return;
    }

    if (strcmp(class_name, "FSModuleConnector") == 0) {
        vxn_swizzle_tracked_method(cls, "probeResource:replyHandler:", (IMP)vxn_fs_module_probe_resource);
        vxn_swizzle_tracked_method(
            cls,
            "loadResource:options:replyHandler:",
            (IMP)vxn_fs_module_load_resource
        );
        vxn_swizzle_tracked_method(
            cls,
            "activateVolume:resource:options:replyHandler:",
            (IMP)vxn_fs_module_activate_volume
        );
        vxn_swizzle_tracked_method(
            cls,
            "getVolumeEndpoint:replyHandler:",
            (IMP)vxn_fs_module_get_volume_endpoint
        );
        return;
    }

    if (strcmp(class_name, "FSVolumeConnector") == 0) {
        vxn_swizzle_tracked_method(cls, "mount:replyHandler:", (IMP)vxn_fs_volume_mount);
        vxn_swizzle_tracked_method(
            cls,
            "getRootFileHandle:",
            (IMP)vxn_fs_volume_get_root_file_handle
        );
        vxn_swizzle_tracked_method(
            cls,
            "getStandardItemAttributesDataForItem:replyHandler:",
            (IMP)vxn_fs_volume_get_standard_item_attributes_data
        );
        vxn_swizzle_tracked_method(
            cls,
            "lookupIn:name:flags:requestID:replyHandler:",
            (IMP)vxn_fs_volume_lookup_in
        );
        vxn_swizzle_tracked_method(
            cls,
            "open:withMode:requestID:replyHandler:",
            (IMP)vxn_fs_volume_open
        );
        vxn_swizzle_tracked_method(
            cls,
            "readFrom:atOffset:intoBuffer:requestID:replyHandler:",
            (IMP)vxn_fs_volume_read_from
        );
    }
}

static void vxn_nsinvocation_invoke_with_target(id self, SEL _cmd, id target) {
    SEL selector_sel = sel_registerName("selector");
    SEL invoked_sel = NULL;
    if (class_respondsToSelector(object_getClass(self), selector_sel)) {
        invoked_sel = ((SEL (*)(id, SEL))objc_msgSend)(self, selector_sel);
    }

    char *target_desc = vxn_copy_nsobject_description(target);
    vxn_log(
        "NSInvocation invokeWithTarget target=%s selector=%s",
        target_desc,
        invoked_sel ? sel_getName(invoked_sel) : "<null>"
    );
    free(target_desc);

    if (vxn_nsinvocation_invoke_with_target_imp != NULL) {
        ((void (*)(id, SEL, id))vxn_nsinvocation_invoke_with_target_imp)(self, _cmd, target);
    }
}

static void vxn_nsxpcinterface_set_classes(
    id self,
    SEL _cmd,
    id classes,
    SEL selector,
    unsigned long long argument_index,
    BOOL of_reply
) {
    char *self_desc = vxn_copy_nsobject_description(self);
    char *classes_desc = vxn_copy_nsobject_description(classes);
    vxn_log(
        "NSXPCInterface setClasses self=%s selector=%s argumentIndex=%llu ofReply=%d classes=%s",
        self_desc,
        selector ? sel_getName(selector) : "<null>",
        argument_index,
        of_reply ? 1 : 0,
        classes_desc
    );
    free(self_desc);
    free(classes_desc);

    if (vxn_nsxpcinterface_set_classes_imp != NULL) {
        ((void (*)(id, SEL, id, SEL, unsigned long long, BOOL))
             vxn_nsxpcinterface_set_classes_imp)(
            self,
            _cmd,
            classes,
            selector,
            argument_index,
            of_reply
        );
    }
}

static void vxn_nsxpcinterface_set_interface(
    id self,
    SEL _cmd,
    id interface,
    SEL selector,
    unsigned long long argument_index,
    BOOL of_reply
) {
    char *self_desc = vxn_copy_nsobject_description(self);
    char *iface_desc = vxn_copy_nsobject_description(interface);
    vxn_log(
        "NSXPCInterface setInterface self=%s selector=%s argumentIndex=%llu ofReply=%d interface=%s",
        self_desc,
        selector ? sel_getName(selector) : "<null>",
        argument_index,
        of_reply ? 1 : 0,
        iface_desc
    );
    free(self_desc);
    free(iface_desc);

    if (vxn_nsxpcinterface_set_interface_imp != NULL) {
        ((void (*)(id, SEL, id, SEL, unsigned long long, BOOL))
             vxn_nsxpcinterface_set_interface_imp)(
            self,
            _cmd,
            interface,
            selector,
            argument_index,
            of_reply
        );
    }
}

static void vxn_nsxpcinterface_set_reply_block_signature(
    id self,
    SEL _cmd,
    const char *signature,
    SEL selector
) {
    char *self_desc = vxn_copy_nsobject_description(self);
    vxn_log(
        "NSXPCInterface setReplyBlockSignature self=%s selector=%s signature=%s",
        self_desc,
        selector ? sel_getName(selector) : "<null>",
        signature ? signature : "<null>"
    );
    free(self_desc);

    if (vxn_nsxpcinterface_set_reply_block_signature_imp != NULL) {
        ((void (*)(id, SEL, const char *, SEL))vxn_nsxpcinterface_set_reply_block_signature_imp)(
            self,
            _cmd,
            signature,
            selector
        );
    }
}

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
    vxn_log_method_list_for_object_once("exportedObject", object);
    vxn_maybe_swizzle_exported_object(object);

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

    IMP original = vxn_lookup_class_imp(vxn_listener_delegate_imps, object_getClass(self));
    BOOL accepted = YES;
    if (original != NULL) {
        accepted = ((BOOL (*)(id, SEL, id, id))original)(self, _cmd, listener, connection);
    }

    vxn_log(
        "listener:shouldAcceptNewConnection: result=%d delegateClass=%s",
        accepted ? 1 : 0,
        class_getName(object_getClass(self))
    );
    vxn_log_connection_state("listener.accept.post", connection);
    return accepted;
}

static void vxn_maybe_swizzle_listener_delegate(id delegate) {
    if (delegate == nil) {
        return;
    }
    Class cls = object_getClass(delegate);
    SEL sel = sel_registerName("listener:shouldAcceptNewConnection:");
    Method method = class_getInstanceMethod(cls, sel);
    if (method == NULL) {
        return;
    }

    IMP current = class_getMethodImplementation(cls, sel);
    if (current == (IMP)vxn_listener_should_accept_new_connection) {
        return;
    }

    vxn_store_class_imp(&vxn_listener_delegate_imps, cls, current);
    const char *types = method_getTypeEncoding(method);
    if (!class_addMethod(cls, sel, (IMP)vxn_listener_should_accept_new_connection, types)) {
        method_setImplementation(method, (IMP)vxn_listener_should_accept_new_connection);
    }
}

static void vxn_swizzle_nsxpc_methods(void) {
    if (vxn_did_swizzle_nsxpc) {
        return;
    }
    vxn_did_swizzle_nsxpc = true;

    Class nsxpc_connection = objc_getClass("NSXPCConnection");
    Class nsxpc_interface = objc_getClass("NSXPCInterface");
    Class nsxpc_listener = objc_getClass("NSXPCListener");
    Class nsinvocation = objc_getClass("NSInvocation");

    vxn_swizzle_method(
        nsxpc_interface,
        sel_registerName("setClasses:forSelector:argumentIndex:ofReply:"),
        (IMP)vxn_nsxpcinterface_set_classes,
        &vxn_nsxpcinterface_set_classes_imp
    );
    vxn_swizzle_method(
        nsxpc_interface,
        sel_registerName("setInterface:forSelector:argumentIndex:ofReply:"),
        (IMP)vxn_nsxpcinterface_set_interface,
        &vxn_nsxpcinterface_set_interface_imp
    );
    vxn_swizzle_method(
        nsxpc_interface,
        sel_registerName("setReplyBlockSignature:forSelector:"),
        (IMP)vxn_nsxpcinterface_set_reply_block_signature,
        &vxn_nsxpcinterface_set_reply_block_signature_imp
    );

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
    vxn_swizzle_method(
        nsinvocation,
        sel_registerName("invokeWithTarget:"),
        (IMP)vxn_nsinvocation_invoke_with_target,
        &vxn_nsinvocation_invoke_with_target_imp
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
